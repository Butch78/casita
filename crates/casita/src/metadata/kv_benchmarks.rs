//! Permanent, correctness-gated benchmarks of the public metadata primitives.
use crate::{
    MetadataChange as Change, MetadataCheck as Check, MetadataCommitResult as Outcome, MetadataKey,
    NamespaceId, Repository, RootName,
};
use bytes::Bytes;
use std::time::Instant;

fn key(value: impl Into<Bytes>) -> MetadataKey {
    MetadataKey::new(NamespaceId::try_from("obrador.v1").unwrap(), value)
}
fn sample(samples: &mut Vec<serde_json::Value>, operation: &str, iteration: usize, start: Instant) {
    samples.push(serde_json::json!({"operation": operation, "iteration": iteration, "nanos": u64::try_from(start.elapsed().as_nanos()).unwrap()}));
}
fn committed(outcome: Outcome) {
    assert!(matches!(outcome, Outcome::Committed { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "performance probe; run through benchmark run metadata-kv"]
async fn benchmark_metadata_kv() {
    let parameter = |name: &str, default: usize| -> usize {
        std::env::var(name)
            .map(|v| v.parse().unwrap())
            .unwrap_or(default)
    };
    let count = parameter("CASITA_PRIMITIVE_ENTRIES", 257);
    let batch = parameter("CASITA_PRIMITIVE_BATCH", 16);
    let iterations = parameter("CASITA_PRIMITIVE_ITERATIONS", 10);
    let value_bytes = parameter("CASITA_KV_VALUE_BYTES", 256);
    let page_size = parameter("CASITA_KV_PAGE_SIZE", 16);
    assert!(count > 0 && batch > 0 && batch <= 1024 && iterations > 0);
    assert!((1..=4096).contains(&value_bytes) && (1..=1024).contains(&page_size));
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::local(directory.path()).await.unwrap();
    let staging = RootName::try_from("staging/benchmark").unwrap();
    let target = repo
        .import(crate::import::BlobImport::new(
            std::io::Cursor::new(b"benchmark payload"),
            staging.clone(),
        ))
        .await
        .unwrap();
    let value = Bytes::from(vec![b'x'; value_bytes]);
    // A fixed reverse-reference fan-in among an independently growing path
    // inventory isolates range seeks from unrelated metadata growth.
    let fanout = 257;
    for first in (0..count).step_by(1024) {
        committed(
            repo.commit(
                vec![],
                (first..(first + 1024).min(count))
                    .map(|i| Change::Set {
                        key: key(format!("paths/{i:016x}")),
                        value: value.clone(),
                    })
                    .collect(),
            )
            .await
            .unwrap(),
        );
    }
    committed(
        repo.commit(
            vec![],
            (0..fanout)
                .map(|i| Change::Set {
                    key: key(format!("referrers/target/{i:016x}")),
                    value: value.clone(),
                })
                .chain([
                    Change::Set {
                        key: key("referrers/target0/neighbor"),
                        value: "excluded".into(),
                    },
                    Change::Set {
                        key: MetadataKey::new(
                            NamespaceId::try_from("other.v1").unwrap(),
                            "referrers/target/0000000000000000",
                        ),
                        value: "excluded".into(),
                    },
                ])
                .collect(),
        )
        .await
        .unwrap(),
    );
    repo.flush().await.unwrap();
    drop(repo);
    let start = Instant::now();
    let repo = Repository::local(directory.path()).await.unwrap();
    let reopen_nanos = start.elapsed().as_nanos();
    let reader = repo.metadata_reader().await.unwrap();
    let prefix = key("referrers/target/");
    let first = reader.scan(&prefix, None, 256).await.unwrap();
    let deep = first.cursor.unwrap();
    let mut samples = Vec::new();
    for iteration in 0..iterations {
        let hit = key(format!("paths/{:016x}", (iteration * 37) % count));
        let start = Instant::now();
        let actual = reader.get(std::slice::from_ref(&hit)).await.unwrap();
        sample(&mut samples, "get-hit", iteration, start);
        assert_eq!(actual, vec![Some(value.clone())]);
        let start = Instant::now();
        let actual = reader.get(&[key("paths/missing")]).await.unwrap();
        sample(&mut samples, "get-miss", iteration, start);
        assert_eq!(actual, vec![None]);
        // The reference is the previous public one-shot path: open a snapshot,
        // read its revision/catalog reference, then dispatch the point read. The current
        // path does one short transaction and reuses only idle connections.
        // Bracket the current eight-idle limit and retain the original
        // investigation's sixteen-idle boundary cases for comparison.
        for width in [1, 7, 8, 9, 15, 16, 17] {
            let order = if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for reference in order {
                let operation = format!(
                    "get-current-{}{width}",
                    if reference { "reference-" } else { "" }
                );
                let start = Instant::now();
                let actual = futures::future::join_all((0..width).map(|_| {
                    let repo = &repo;
                    let hit = &hit;
                    async move {
                        if reference {
                            repo.metadata_reader()
                                .await
                                .unwrap()
                                .get(std::slice::from_ref(hit))
                                .await
                                .unwrap()
                        } else {
                            repo.get(std::slice::from_ref(hit)).await.unwrap()
                        }
                    }
                }))
                .await;
                sample(&mut samples, &operation, iteration, start);
                assert!(actual.iter().all(|row| row == &vec![Some(value.clone())]));
            }
        }
        let keys: Vec<_> = (0..batch)
            .map(|i| {
                if i % 4 == 0 {
                    key("paths/missing")
                } else {
                    key(format!("paths/{:016x}", (i / 2 + iteration * 37) % count))
                }
            })
            .collect();
        let wanted: Vec<_> = (0..batch)
            .map(|i| {
                if i % 4 == 0 {
                    None
                } else {
                    Some(value.clone())
                }
            })
            .collect();
        // Same keys and snapshot: the scalar control measures the benefit of
        // batching worker dispatch and statement setup, not different data.
        for (op, scalar) in [("get-batch", false), ("get-batch-scalar", true)] {
            let start = Instant::now();
            let actual = if scalar {
                let mut values = Vec::new();
                for key in &keys {
                    values.extend(reader.get(std::slice::from_ref(key)).await.unwrap());
                }
                values
            } else {
                reader.get(&keys).await.unwrap()
            };
            sample(&mut samples, op, iteration, start);
            assert_eq!(actual, wanted);
        }
        for (operation, cursor, offset) in
            [("scan-first", None, 0), ("scan-deep", Some(&deep), 256)]
        {
            let start = Instant::now();
            let page = reader.scan(&prefix, cursor, page_size).await.unwrap();
            sample(&mut samples, operation, iteration, start);
            assert_eq!(page.records.len(), page_size.min(fanout - offset));
            for (i, record) in page.records.iter().enumerate() {
                assert_eq!(
                    record.key,
                    key(format!("referrers/target/{:016x}", i + offset))
                );
                assert_eq!(record.value, value);
            }
        }
        let start = Instant::now();
        let mut cursor = None;
        let mut records = Vec::new();
        loop {
            let page = reader
                .scan(&prefix, cursor.as_ref(), page_size)
                .await
                .unwrap();
            records.extend(page.records);
            cursor = page.cursor;
            if cursor.is_none() {
                break;
            }
        }
        sample(&mut samples, "scan-prefix", iteration, start);
        assert_eq!(records.len(), fanout);
        for (i, record) in records.iter().enumerate() {
            assert_eq!(record.key, key(format!("referrers/target/{i:016x}")));
            assert_eq!(record.value, value);
        }
    }
    drop(reader);
    let root = RootName::try_from("roots/benchmark").unwrap();
    for iteration in 0..iterations {
        let keys: Vec<_> = (0..batch)
            .map(|i| key(format!("realisations/{iteration:08x}/{i:08x}")))
            .collect();
        for (operation, expected, next) in [
            ("commit-insert", None, Some(value.clone())),
            (
                "commit-replace",
                Some(value.clone()),
                Some(Bytes::from("replacement")),
            ),
            ("commit-delete", Some(Bytes::from("replacement")), None),
        ] {
            let checks = keys
                .iter()
                .map(|key| Check::Record {
                    key: key.clone(),
                    expected: expected.clone(),
                })
                .collect();
            let changes = keys
                .iter()
                .map(|key| match &next {
                    Some(value) => Change::Set {
                        key: key.clone(),
                        value: value.clone(),
                    },
                    None => Change::Delete { key: key.clone() },
                })
                .collect();
            let start = Instant::now();
            let outcome = repo.commit(checks, changes).await.unwrap();
            sample(&mut samples, operation, iteration, start);
            committed(outcome);
            assert_eq!(repo.get(&keys).await.unwrap(), vec![next; batch]);
        }
        for (operation, mismatch) in [
            ("commit-conflict-first", 0),
            ("commit-conflict-last", batch - 1),
        ] {
            let mut checks: Vec<_> = keys
                .iter()
                .map(|key| Check::Record {
                    key: key.clone(),
                    expected: None,
                })
                .collect();
            checks[mismatch] = Check::Record {
                key: keys[mismatch].clone(),
                expected: Some(value.clone()),
            };
            let changes = keys
                .iter()
                .map(|key| Change::Set {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect();
            let revision = repo.metadata_reader().await.unwrap().revision();
            let start = Instant::now();
            let outcome = repo.commit(checks, changes).await.unwrap();
            sample(&mut samples, operation, iteration, start);
            assert_eq!(
                outcome,
                Outcome::Conflict {
                    check_index: mismatch
                }
            );
            assert_eq!(repo.metadata_reader().await.unwrap().revision(), revision);
            assert_eq!(repo.get(&keys).await.unwrap(), vec![None; batch]);
        }
        let descriptor = key(format!("paths/registered/{iteration:08x}"));
        let mut changes = vec![
            Change::Set {
                key: descriptor.clone(),
                value: value.clone(),
            },
            Change::SetRoot {
                name: root.clone(),
                target: target.clone(),
            },
        ];
        let edges: Vec<_> = (0..batch)
            .map(|i| key(format!("referrers/registered/{iteration:08x}/{i:08x}")))
            .collect();
        changes.extend(edges.iter().map(|key| Change::Set {
            key: key.clone(),
            value: descriptor.key.clone(),
        }));
        let held = repo.metadata_reader().await.unwrap();
        let start = Instant::now();
        let outcome = repo
            .commit(
                vec![Check::Record {
                    key: descriptor.clone(),
                    expected: None,
                }],
                changes,
            )
            .await
            .unwrap();
        sample(&mut samples, "commit-register-root", iteration, start);
        committed(outcome);
        assert_eq!(
            held.get(std::slice::from_ref(&descriptor)).await.unwrap(),
            vec![None]
        );
        assert_eq!(held.get(&edges).await.unwrap(), vec![None; batch]);
        drop(held);
        assert_eq!(
            repo.get(std::slice::from_ref(&descriptor)).await.unwrap(),
            vec![Some(value.clone())]
        );
        assert_eq!(
            repo.get(&edges).await.unwrap(),
            vec![Some(descriptor.key); batch]
        );
        assert_eq!(repo.root(&root).await.unwrap(), Some(target.clone()));
        for shared in [false, true] {
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let mut tasks = Vec::new();
            let start = Instant::now();
            for writer in 0..2 {
                let repo = repo.clone();
                let barrier = barrier.clone();
                let record = key(format!(
                    "writers/{iteration}/{shared}/{}",
                    if shared { 0 } else { writer }
                ));
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    repo.commit(
                        vec![Check::Record {
                            key: record.clone(),
                            expected: None,
                        }],
                        vec![Change::Set {
                            key: record,
                            value: "won".into(),
                        }],
                    )
                    .await
                    .unwrap()
                }));
            }
            let mut outcomes = Vec::new();
            for task in tasks {
                outcomes.push(task.await.unwrap());
            }
            sample(
                &mut samples,
                if shared {
                    "commit-contended"
                } else {
                    "commit-disjoint"
                },
                iteration,
                start,
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|o| matches!(o, Outcome::Committed { .. }))
                    .count(),
                if shared { 1 } else { 2 }
            );
        }
    }
    repo.flush().await.unwrap();
    drop(repo);
    let repo = Repository::local(directory.path()).await.unwrap();
    let reader = repo.metadata_reader().await.unwrap();
    // Audit every indexed row after reopen without reconstructing an index.
    let mut cursor = None;
    let mut total = 0;
    loop {
        let page = reader.scan(&key(""), cursor.as_ref(), 1024).await.unwrap();
        total += page.records.len();
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(total, count + fanout + 1 + iterations * (batch + 1 + 3));
    drop(reader);
    repo.flush().await.unwrap();
    assert_eq!(repo.collect().await.unwrap().logical_objects, 0);
    committed(
        repo.commit(
            vec![],
            vec![
                Change::RemoveRoot { name: root },
                Change::RemoveRoot { name: staging },
            ],
        )
        .await
        .unwrap(),
    );
    assert_eq!(repo.collect().await.unwrap().logical_objects, 1);
    assert!(repo.object(&target).await.unwrap().is_none());
    assert!(repo.get(&[key("paths/registered/00000000")]).await.unwrap()[0].is_some());
    repo.flush().await.unwrap();
    println!(
        "primitive_sample {}",
        serde_json::json!({
            "count": count, "batch": batch, "iterations": iterations, "value_bytes": value_bytes,
            "page_size": page_size, "fanout": fanout, "reopen_nanos": u64::try_from(reopen_nanos).unwrap(), "samples": samples,
            "correctness": "exact values and prefix pages, atomic checks and roots, concurrent winners, persisted indexes independent of GC"
        })
    );
}
