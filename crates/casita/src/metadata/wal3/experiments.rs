//! Isolated physical checkpoint experiments. These do not change WAL policy.

use super::*;

fn record(index: u32) -> ObjectRecord {
    let key = ObjectKey::new(
        crate::NamespaceId::try_from("benchmark.logical.v1").unwrap(),
        index.to_be_bytes().to_vec(),
    )
    .unwrap();
    let payload = crate::BlobId::new(crate::Digest::hash(&index.to_le_bytes()));
    ObjectRecord::new(key, payload, 4, Vec::new()).unwrap()
}

/// Real shard reads, encodings, and writes with synthetic evenly spread
/// additions. WAL network operations are deliberately outside this experiment.
#[tokio::test]
#[ignore = "release-mode checkpoint amplification experiment"]
async fn checkpoint_window_amplification() {
    let groups: u32 = std::env::var("CASITA_CHECKPOINT_GROUPS")
        .ok()
        .map(|n| n.parse().unwrap())
        .unwrap_or(16);
    let entries: u32 = 1024;
    let generations: u32 = 128;
    for interval in [9u32, 64, 128] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(chroma_storage::Storage::Local(
            chroma_storage::local::LocalStorage::new(directory.path().to_str().unwrap()),
        ));
        let shards = ObjectShardStorage::new(storage, "experiment".to_owned(), 0);
        let mut map = StateShardMap::default();
        for group in 0..groups {
            let objects = (0..entries)
                .map(|n| (record(group * 1_000_000 + n * 512), false, 0))
                .collect::<Vec<_>>();
            let encoded = encode_object_shard(&objects).unwrap();
            shards.put(&encoded).await.unwrap();
            append_object_shard_reference(&mut map, encoded.reference);
        }
        let mut state = StateData::empty().unwrap();
        state.base_objects = Arc::new(map);
        shards.reset_stats();
        let started = Instant::now();
        let mut written_bytes = 0u64;
        let mut checkpoints = 0u32;
        let mut peak_overlay_bytes = 0usize;
        for generation in 0..generations {
            for group in 0..groups {
                let object = record(group * 1_000_000 + generation * 2 + 1);
                state
                    .births
                    .insert(object.key().clone(), u64::from(generation) + 1);
                state.generation = u64::from(generation) + 1;
                state.objects.insert(object.key().clone(), object);
            }
            let overlay_bytes = state
                .objects
                .values()
                .map(|record| record.encode().len())
                .sum();
            peak_overlay_bytes = peak_overlay_bytes.max(overlay_bytes);
            if (generation + 1) % interval == 0 || generation + 1 == generations {
                let before = state
                    .base_objects
                    .objects
                    .iter()
                    .map(|shard| shard.digest)
                    .collect::<BTreeSet<_>>();
                state = compact_state_objects(&shards, &state).await.unwrap();
                written_bytes += state
                    .base_objects
                    .objects
                    .iter()
                    .filter(|shard| !before.contains(&shard.digest))
                    .map(|shard| shard.encoded_bytes)
                    .sum::<u64>();
                checkpoints += 1;
            }
        }
        let nanos = started.elapsed().as_nanos();
        let stats = shards.stats();
        assert_eq!(
            state.base_objects.object_count,
            u64::from(groups * (entries + generations))
        );
        for group in 0..groups {
            for generation in [0, generations - 1] {
                let expected = record(group * 1_000_000 + generation * 2 + 1);
                assert_eq!(
                    shards
                        .lookup(state.base_objects.as_ref(), expected.key())
                        .await
                        .unwrap()
                        .unwrap()
                        .0,
                    expected
                );
            }
        }
        println!(
            "checkpoint-experiment {}",
            serde_json::json!({
                "interval": interval, "groups": groups, "initial_objects": groups * entries,
                "generations": generations, "added_objects": groups * generations,
                "checkpoints": checkpoints, "shard_get_requests": stats.get_requests,
                "shard_get_bytes": stats.get_bytes, "shard_put_requests": stats.put_requests,
                "shard_put_bytes": written_bytes, "peak_overlay_encoded_bytes": peak_overlay_bytes,
                "wall_nanos": nanos, "validated": true,
            })
        );
    }
}
