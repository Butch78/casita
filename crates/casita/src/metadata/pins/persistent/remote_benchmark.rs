//! Counts the production optimistic edit path and wire codec, without network.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct Counts {
    gets: AtomicU64,
    puts: AtomicU64,
    read_bytes: AtomicU64,
    write_bytes: AtomicU64,
}

struct Counted {
    state: tokio::sync::Mutex<PinInventory>,
    counts: Counts,
}

#[async_trait]
impl Backend for Counted {
    type Version = u64;

    async fn load(&self) -> Result<(PinInventory, u64), MetadataError> {
        let state = self.state.lock().await;
        let bytes = codec::encode(&state)?;
        self.counts.gets.fetch_add(1, Ordering::Relaxed);
        self.counts
            .read_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok((codec::decode(&bytes)?, state.revision))
    }

    async fn compare_exchange(
        &self,
        expected: u64,
        state: PinInventory,
    ) -> Result<bool, MetadataError> {
        let bytes = codec::encode(&state)?;
        self.counts.puts.fetch_add(1, Ordering::Relaxed);
        self.counts
            .write_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let mut current = self.state.lock().await;
        if current.revision != expected {
            return Ok(false);
        }
        *current = codec::decode(&bytes)?;
        Ok(true)
    }
}

#[tokio::test]
#[ignore = "permanent remote pin request/codec benchmark"]
async fn benchmark_remote_pin_cost() {
    // These are active pins in one process, deliberately not advertised as
    // independent processes or a latency/throughput comparison.
    for writers in [1, 10, 32, 64, 100] {
        for resources in [7, 8, 9, 64] {
            for batch in [1, 8] {
                let store = Counted {
                    state: Default::default(),
                    counts: Counts::default(),
                };
                let mut tokens = Vec::new();
                for _ in 0..writers {
                    tokens.push(
                        store
                            .register(DataPin {
                                scope: PinScope::Staging,
                                catalog: None,
                                resources: BTreeSet::new(),
                            })
                            .await
                            .unwrap()
                            .unwrap(),
                    );
                }
                for offset in (0..resources).step_by(batch) {
                    for (owner, token) in tokens.iter().enumerate() {
                        let additions = (offset..(offset + batch).min(resources))
                            .map(|index| {
                                PinResource::StorageObject(format!("objects/{owner:04}/{index:08}"))
                            })
                            .collect();
                        assert!(store.protect(token, additions).await.unwrap());
                    }
                }
                let inventory = store.state.lock().await.clone();
                assert_eq!(inventory.pins.len(), writers);
                for pin in inventory.pins.values() {
                    assert_eq!(pin.resources.len(), resources);
                }
                for token in tokens {
                    store.release(&token).await.unwrap();
                }
                assert!(store.state.lock().await.pins.is_empty());
                let expected = (writers * (2 + resources.div_ceil(batch))) as u64;
                assert_eq!(store.counts.gets.load(Ordering::Relaxed), expected);
                assert_eq!(store.counts.puts.load(Ordering::Relaxed), expected);
                println!(
                    "remote_pin_cost_sample {}",
                    serde_json::json!({
                        "writers": writers, "resources": resources, "batch": batch,
                        "gets": expected, "puts": expected,
                        "read_bytes": store.counts.read_bytes.load(Ordering::Relaxed),
                        "write_bytes": store.counts.write_bytes.load(Ordering::Relaxed),
                        "correctness": "production codec roundtrip; complete inventory; release",
                    })
                );
            }
        }
    }
}
