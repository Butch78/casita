//! Baselines for the existing metadata contract, not the proposed mutable KV API.
use super::*;
use crate::digest::{BlobId, Digest};
use crate::format::{FormatLimits, FormatRegistry};
use futures::TryStreamExt;
use std::io::Cursor;
use std::time::Instant;

async fn blob(index: usize) -> VerifiedObject {
    let bytes = (index as u64).to_le_bytes();
    let key = ObjectKey::blob(BlobId::new(Digest::hash(&bytes)));
    FormatRegistry::builtin()
        .verify(&key, &mut Cursor::new(bytes), &FormatLimits::default())
        .await
        .unwrap()
}

fn sample(samples: &mut Vec<serde_json::Value>, operation: &str, iteration: usize, nanos: u128) {
    samples.push(serde_json::json!({
        "operation": operation, "iteration": iteration, "nanos": u64::try_from(nanos).unwrap()
    }));
}

#[tokio::test]
#[ignore = "performance probe; run through benchmark run metadata-primitives"]
async fn benchmark_metadata_primitives() {
    let parameter = |name: &str, default: usize| -> usize {
        std::env::var(name)
            .map(|v| v.parse().unwrap())
            .unwrap_or(default)
    };
    let count = parameter("CASITA_PRIMITIVE_ENTRIES", 257);
    let batch = parameter("CASITA_PRIMITIVE_BATCH", 16);
    let iterations = parameter("CASITA_PRIMITIVE_ITERATIONS", 3);
    assert!(count > 0 && batch > 0 && iterations > 0);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.sqlite");
    let store = TursoMetadataStore::open(&path).await.unwrap();
    let mut revision = store.snapshot().await.unwrap().revision();
    let mut expected = Vec::new();
    for first in (0..count).step_by(1024) {
        let mut mutation = MetadataMutation::new();
        for index in first..(first + 1024).min(count) {
            let object = blob(index).await;
            expected.push(object.record().clone());
            mutation.add_object(object);
        }
        revision = store.commit(&revision, mutation).await.unwrap().revision;
    }
    expected.sort_by(|a, b| a.key().cmp(b.key()));
    drop(store);
    let store = TursoMetadataStore::open(&path).await.unwrap();
    let snapshot = store.snapshot().await.unwrap();
    assert_eq!(snapshot.revision(), revision);
    assert!(
        snapshot
            .roots()
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty()
    );
    let mut samples = Vec::new();
    let missing = blob(usize::MAX).await.record().key().clone();
    for iteration in 0..iterations {
        // Rotate across the inventory; duplicate keys and misses retain input order.
        let hit = &expected[(iteration * 37) % count];
        let started = Instant::now();
        let actual = snapshot.object(hit.key()).await.unwrap();
        sample(
            &mut samples,
            "get-hit",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(actual.as_ref(), Some(hit));
        let started = Instant::now();
        let actual = snapshot.object(&missing).await.unwrap();
        sample(
            &mut samples,
            "get-miss",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(actual, None);
        for kind in ["hit", "miss", "mixed"] {
            let wanted: Vec<_> = (0..batch)
                .map(|index| {
                    if kind == "miss" || (kind == "mixed" && index % 2 == 0) {
                        None
                    } else {
                        Some(expected[(iteration * 37 + index / 2) % count].clone())
                    }
                })
                .collect();
            let keys: Vec<_> = wanted
                .iter()
                .map(|record| {
                    record
                        .as_ref()
                        .map_or_else(|| missing.clone(), |record| record.key().clone())
                })
                .collect();
            let started = Instant::now();
            let actual = snapshot.object_batch(&keys).await.unwrap();
            sample(
                &mut samples,
                &format!("get-batch-{kind}"),
                iteration,
                started.elapsed().as_nanos(),
            );
            assert_eq!(actual, wanted);
        }
        let started = Instant::now();
        let actual = snapshot.objects().try_collect::<Vec<_>>().await.unwrap();
        sample(
            &mut samples,
            "scan-all",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(actual, expected);
        assert_eq!(snapshot.revision(), revision);
    }
    drop(snapshot);
    let mut inventory: BTreeMap<_, _> =
        expected.into_iter().map(|r| (r.key().clone(), r)).collect();
    let root = RootName::try_from("roots/primitive-benchmark").unwrap();
    for iteration in 0..iterations {
        let before = store.snapshot().await.unwrap();
        let previous_root = before.root(&root).await.unwrap();
        let mut mutation = MetadataMutation::new();
        let mut additions = Vec::new();
        for index in 0..batch {
            let object = blob(count + iteration * batch + index).await;
            additions.push(object.record().clone());
            mutation.add_object(object);
        }
        let target = additions[0].key().clone();
        mutation.set_root(root.clone(), target.clone());
        let started = Instant::now();
        let committed = store.commit(&revision, mutation).await.unwrap();
        sample(
            &mut samples,
            "commit-insert-root",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(committed.objects_inserted, batch);
        assert_eq!(committed.roots_changed, 1);
        // A held read sees neither half of the new publication.
        assert_eq!(before.root(&root).await.unwrap(), previous_root);
        assert_eq!(before.object(&target).await.unwrap(), None);
        assert_eq!(before.revision(), revision);
        drop(before);
        let after = store.snapshot().await.unwrap();
        assert_eq!(after.root(&root).await.unwrap(), Some(target.clone()));
        for record in &additions {
            assert_eq!(
                after.object(record.key()).await.unwrap().as_ref(),
                Some(record)
            );
        }
        drop(after);
        let rejected = blob(count + iterations * batch + iteration).await;
        let rejected_key = rejected.record().key().clone();
        let mut conflict = MetadataMutation::new();
        conflict.add_object(rejected).remove_root(root.clone());
        let started = Instant::now();
        let outcome = store.commit(&revision, conflict).await;
        sample(
            &mut samples,
            "commit-stale",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert!(matches!(outcome, Err(MetadataError::StaleRevision { .. })));
        revision = committed.revision;
        let after = store.snapshot().await.unwrap();
        assert_eq!(after.revision(), revision);
        assert_eq!(after.root(&root).await.unwrap(), Some(target));
        assert_eq!(after.object(&rejected_key).await.unwrap(), None);
        drop(after);
        // A durable root forbids pruning its target; failed collection must
        // leave the revision and the inventory intact.
        assert!(matches!(
            store
                .commit(
                    &revision,
                    MetadataMutation::install_retained_objects(BTreeSet::new())
                )
                .await,
            Err(MetadataError::InvalidRetainedSet(_))
        ));
        let replacement = inventory.first_key_value().unwrap().0.clone();
        let mut repoint = MetadataMutation::new();
        repoint.set_root(root.clone(), replacement.clone());
        let started = Instant::now();
        let committed = store.commit(&revision, repoint).await.unwrap();
        sample(
            &mut samples,
            "commit-repoint-root",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(committed.roots_changed, 1);
        revision = committed.revision;
        assert_eq!(
            store.snapshot().await.unwrap().root(&root).await.unwrap(),
            Some(replacement)
        );
        let mut remove = MetadataMutation::new();
        remove.remove_root(root.clone());
        let started = Instant::now();
        let committed = store.commit(&revision, remove).await.unwrap();
        sample(
            &mut samples,
            "commit-remove-root",
            iteration,
            started.elapsed().as_nanos(),
        );
        assert_eq!(committed.roots_changed, 1);
        revision = committed.revision;
        inventory.extend(additions.into_iter().map(|r| (r.key().clone(), r)));
    }
    drop(store);
    let store = TursoMetadataStore::open(&path).await.unwrap();
    let snapshot = store.snapshot().await.unwrap();
    assert_eq!(snapshot.revision(), revision);
    assert_eq!(
        snapshot.objects().try_collect::<Vec<_>>().await.unwrap(),
        inventory.into_values().collect::<Vec<_>>()
    );
    assert!(
        snapshot
            .roots()
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty()
    );
    drop(snapshot);
    // Presence in the object inventory alone does not retain an object. This is
    // metadata pruning, not a claim about arbitrary indexes or payload GC.
    let committed = store
        .commit(
            &revision,
            MetadataMutation::install_retained_objects(BTreeSet::new()),
        )
        .await
        .unwrap();
    assert_eq!(committed.objects_removed, count + iterations * batch);
    let snapshot = store.snapshot().await.unwrap();
    assert!(
        snapshot
            .objects()
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty()
    );
    println!(
        "primitive_sample {}",
        serde_json::json!({
            "count": count, "batch": batch, "iterations": iterations, "samples": samples,
            "correctness": "exact reads, atomic revision CAS, stable snapshots, reopened inventory, unrooted pruning"
        })
    );
}
