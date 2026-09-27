//! Permanent durable-ledger latency and contention probes.
use super::*;
use std::time::Instant;

fn staging(label: impl Into<String>) -> DataPin {
    DataPin {
        scope: PinScope::Staging,
        catalog: None,
        resources: BTreeSet::from([PinResource::StorageObject(label.into())]),
    }
}

fn phase_metrics(
    store: &FilePinStore,
    before: &BTreeMap<&'static str, u64>,
) -> BTreeMap<&'static str, u64> {
    store
        .local()
        .unwrap()
        .stats
        .snapshot()
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                if name == "max_group" {
                    value
                } else {
                    value - before[name]
                },
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "permanent growing staging pin benchmark; run benchmark pin-growth"]
async fn benchmark_pin_growth() {
    let count: usize = std::env::var("CASITA_PIN_RESOURCES")
        .unwrap()
        .parse()
        .unwrap();
    let iterations: usize = std::env::var("CASITA_PIN_ITERATIONS")
        .unwrap()
        .parse()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = FilePinStore::new(directory.path().join("pins"));
    // Fixed-width resources put the 1 MiB journal window between 8K and
    // 16K resources. Seed outside timing; protect grows this same staging pin.
    let resource = |i: usize| PinResource::StorageObject(format!("objects/{i:064x}"));
    let mut expected: BTreeSet<_> = (0..count).map(resource).collect();
    let token = store
        .register(DataPin {
            scope: PinScope::Staging,
            catalog: None,
            resources: expected.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    let before = store.local().unwrap().stats.snapshot();
    let (mut replay_operations, mut replay_bytes) = {
        let local = store.local().unwrap();
        let cache = local.cache.lock().unwrap();
        cache.replay_position()
    };
    let mut expected_checkpoints = 0;
    let mut nanos = Vec::new();
    for i in count..count + iterations {
        let additions = BTreeSet::from([resource(i)]);
        let start = Instant::now();
        assert!(store.protect(&token, additions.clone()).await.unwrap());
        nanos.push(start.elapsed().as_nanos() as u64);
        expected.extend(additions);
        // Each fixed-width addition fits one block regardless of pin size.
        if replay_operations + 1 > super::journal::CHECKPOINT_OPERATIONS
            || replay_bytes + super::journal::BLOCK > super::journal::WINDOW
        {
            expected_checkpoints += 1;
            replay_operations = 0;
            replay_bytes = 0;
        } else {
            replay_operations += 1;
            replay_bytes += super::journal::BLOCK;
        }
    }
    let metrics = phase_metrics(&store, &before);
    assert_eq!(metrics["checkpoints"], expected_checkpoints);
    assert_eq!(
        metrics["journal_frames"],
        iterations as u64 - expected_checkpoints
    );
    assert_eq!(
        metrics["journal_syncs"],
        iterations as u64 + expected_checkpoints
    );
    // Force replay from disk, then test arbitration against the grown pin.
    *store.local().unwrap().cache.lock().unwrap() = Default::default();
    let reopened = FilePinStore::new(&store.path);
    let state = reopened.inventory().await.unwrap();
    assert_eq!(state.pins.len(), 1);
    assert_eq!(state.pins[&token].resources, expected);
    assert!(
        reopened
            .claim_deletions(state.revision, BTreeSet::from([resource(count)]))
            .await
            .unwrap()
            .is_none()
    );
    reopened.release(&token).await.unwrap();
    let state = reopened.inventory().await.unwrap();
    assert!(state.pins.is_empty());
    let claim = reopened
        .claim_deletions(state.revision, BTreeSet::from([resource(count)]))
        .await
        .unwrap()
        .unwrap();
    reopened.finish_deletions(&claim).await.unwrap();
    assert!(reopened.inventory().await.unwrap().deletions.is_empty());
    println!(
        "pin_growth_sample {}",
        serde_json::json!({
            "resources": count, "iterations": iterations, "nanos": nanos, "metrics": metrics,
            "correctness": "exact replayed resources; protected deletion rejected; release permits deletion; no leaked pins or claims",
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "permanent durable ledger benchmark; run benchmark durable-ledger"]
async fn benchmark_durable_ledger() {
    let count: usize = std::env::var("CASITA_LEDGER_RECORDS")
        .unwrap()
        .parse()
        .unwrap();
    let writers: usize = std::env::var("CASITA_LEDGER_WRITERS")
        .unwrap()
        .parse()
        .unwrap();
    let iterations: usize = std::env::var("CASITA_LEDGER_ITERATIONS")
        .unwrap()
        .parse()
        .unwrap();
    let mode = std::env::var("CASITA_LEDGER_MODE").unwrap();
    assert!(matches!(mode.as_str(), "journal" | "replacement"));
    let context = std::env::var("CASITA_LEDGER_CONTEXT").unwrap_or_else(|_| "quiet".into());
    assert!(matches!(context.as_str(), "quiet" | "readers" | "claims"));
    let dir = tempfile::tempdir().unwrap();
    let mut store = FilePinStore::new(dir.path().join("pins"));
    store.replacement = mode == "replacement";
    let mut seed = PinInventory {
        revision: 1,
        ..Default::default()
    };
    for index in 0..count {
        seed.pins.insert(
            PinToken::fresh().unwrap(),
            staging(format!("retained/{index}")),
        );
    }
    if context == "claims" {
        for index in 0..64 {
            seed.deletions.insert(
                PinToken::fresh().unwrap(),
                BTreeSet::from([PinResource::StorageObject(format!("claimed/{index}"))]),
            );
        }
    }
    store.write_locked(&codec::encode(&seed).unwrap()).unwrap();
    let mut readers = Vec::new();
    if context == "readers" {
        for index in 0..64 {
            readers.push(
                store
                    .register_reader(DataPin {
                        scope: PinScope::Snapshot { generation: 0 },
                        catalog: None,
                        resources: BTreeSet::from([PinResource::StorageObject(format!(
                            "reader/{index}"
                        ))]),
                    })
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    let expected_pins = store.inventory().await.unwrap().pins;
    if context == "claims" {
        assert!(
            store
                .register(staging("claimed/0"))
                .await
                .unwrap()
                .is_none()
        );
    }
    if context == "readers" {
        let revision = store.inventory().await.unwrap().revision;
        assert!(
            store
                .claim_deletions(
                    revision,
                    BTreeSet::from([PinResource::StorageObject("reader/0".into())])
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut samples = Vec::new();
    for iteration in 0..iterations {
        let baseline = store.local().unwrap().stats.snapshot();
        let start = Instant::now();
        let tokens = futures::future::join_all((0..writers).map(|writer| {
            let store = &store;
            async move {
                store
                    .register(staging(format!("staging/{iteration}/{writer}")))
                    .await
                    .unwrap()
                    .unwrap()
            }
        }))
        .await;
        samples.push(serde_json::json!({"phase":"register", "iteration":iteration, "nanos":start.elapsed().as_nanos() as u64, "operations":writers,"metrics":phase_metrics(&store,&baseline)}));
        for (phase, publication) in [("payload-protect", false), ("publication-protect", true)] {
            let baseline = store.local().unwrap().stats.snapshot();
            let start = Instant::now();
            futures::future::join_all(tokens.iter().enumerate().map(|(writer, token)| {
                let store = &store;
                async move {
                    let resource = if publication {
                        PinResource::Catalog(format!("catalog/{iteration}/{writer}").into_bytes())
                    } else {
                        PinResource::StorageObject(format!("pack/{iteration}/{writer}"))
                    };
                    assert!(
                        store
                            .protect(token, BTreeSet::from([resource]))
                            .await
                            .unwrap()
                    );
                }
            }))
            .await;
            samples.push(serde_json::json!({"phase":phase,"iteration":iteration,"nanos":start.elapsed().as_nanos() as u64,"operations":writers,"metrics":phase_metrics(&store,&baseline)}));
        }
        let admitted = store.inventory().await.unwrap();
        assert_eq!(admitted.pins.len(), count + readers.len() + writers);
        for token in &tokens {
            assert_eq!(admitted.pins[token].resources.len(), 3);
        }
        assert!(
            store
                .claim_deletions(
                    seed.revision,
                    BTreeSet::from([PinResource::StorageObject("unrelated".into())])
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .claim_deletions(
                    admitted.revision,
                    admitted.pins[&tokens[0]].resources.clone()
                )
                .await
                .unwrap()
                .is_none()
        );
        let baseline = store.local().unwrap().stats.snapshot();
        let start = Instant::now();
        futures::future::join_all(tokens.iter().map(|token| store.release(token)))
            .await
            .into_iter()
            .for_each(|result| result.unwrap());
        samples.push(serde_json::json!({"phase":"release","iteration":iteration,"nanos":start.elapsed().as_nanos() as u64,"operations":writers,"metrics":phase_metrics(&store,&baseline)}));
        let inventory = store.inventory().await.unwrap();
        assert_eq!(inventory.pins, expected_pins);
        let baseline = store.local().unwrap().stats.snapshot();
        let start = Instant::now();
        let claim = store
            .claim_deletions(
                inventory.revision,
                BTreeSet::from([PinResource::StorageObject("unrelated".into())]),
            )
            .await
            .unwrap()
            .unwrap();
        samples.push(serde_json::json!({"phase":"deletion-claim","iteration":iteration,"nanos":start.elapsed().as_nanos() as u64,"operations":1,"metrics":phase_metrics(&store,&baseline)}));
        assert!(
            store
                .register(staging("unrelated"))
                .await
                .unwrap()
                .is_none()
        );
        let baseline = store.local().unwrap().stats.snapshot();
        let start = Instant::now();
        store.finish_deletions(&claim).await.unwrap();
        samples.push(serde_json::json!({"phase":"deletion-finish","iteration":iteration,"nanos":start.elapsed().as_nanos() as u64,"operations":1,"metrics":phase_metrics(&store,&baseline)}));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            *store.local().unwrap().cache.lock().unwrap() = Default::default();
        }
        let reopened = FilePinStore::new(&store.path).inventory().await.unwrap();
        assert_eq!(reopened.pins, expected_pins);
        assert_eq!(reopened.deletions, seed.deletions);
    }
    for token in readers {
        store.release(&token).await.unwrap();
    }
    assert_eq!(store.inventory().await.unwrap().pins, seed.pins);
    println!(
        "durable_ledger_sample {}",
        serde_json::json!({"records":count,"writers":writers,"iterations":iterations,"mode":mode,"context":context,"samples":samples,
        "correctness":"exact retained inventory, staged and publication resources protected, stale and protected claims rejected, no leaked pins or claims"})
    );
}
