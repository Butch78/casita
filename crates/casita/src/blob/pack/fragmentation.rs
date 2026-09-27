//! Permanent packing/reconstruction probe; no network or repository metadata.
use super::*;
use crate::blob::{BlobStore, ChunkedBlobStore, DEFAULT_AVG_CHUNK_SIZE};
use object_store::memory::InMemory;
use serde_json::json;
use std::num::NonZeroUsize;
use tokio::io::AsyncReadExt;

const PACK_BYTES: u64 = 1024 * 1024;
const FILE_BYTES: usize = 16 * 1024 * 1024;

async fn open(objects: Arc<dyn ObjectStore>, cache: u64) -> ChunkedBlobStore {
    ChunkedBlobStore::packed_with_options(
        objects,
        Path::default(),
        DEFAULT_AVG_CHUNK_SIZE,
        crate::PackOptions {
            target_size: PACK_BYTES,
            cache_capacity: cache,
        },
    )
    .await
    .unwrap()
    .with_chunk_upload_concurrency(NonZeroUsize::new(1).unwrap())
}

fn random(label: u64, bytes: &mut [u8]) {
    blake3::Hasher::new()
        .update(b"casita-fragmentation-v1")
        .update(&label.to_le_bytes())
        .finalize_xof()
        .fill(bytes);
}

async fn audit(store: &ChunkedBlobStore, id: &BlobId, expected: &[u8]) -> u64 {
    let start = Instant::now();
    let mut reader = store.open_read(id).await.unwrap().unwrap();
    let mut actual = Vec::with_capacity(expected.len());
    reader.read_to_end(&mut actual).await.unwrap();
    let nanos = start.elapsed().as_nanos() as u64;
    assert_eq!(actual, expected);
    assert_eq!(*id, BlobId::new(blake3::hash(&actual).into()));
    nanos
}

async fn measure(
    objects: Arc<dyn ObjectStore>,
    expected: &[u8],
    pattern: &str,
    generation: usize,
    layout: &str,
) {
    let id = BlobId::new(blake3::hash(expected).into());
    let store = open(objects.clone(), 0).await;
    let chunks = store.chunks(&id).await.unwrap().unwrap();
    let catalog = PackedChunks::open(objects.clone(), Path::default(), PACK_BYTES)
        .await
        .unwrap();
    let mut packs = BTreeMap::new();
    let mut runs = 0;
    let mut previous = None;
    for chunk in &chunks {
        let location = catalog.location(&chunk.digest).await.unwrap().unwrap();
        if previous != Some(location.pack) {
            runs += 1;
        }
        previous = Some(location.pack);
        packs.insert(location.pack, location.pack_len);
    }
    let working_bytes: u64 = packs.values().sum();
    let largest = *packs.values().max().unwrap();
    drop(catalog);
    drop(store);
    let inventory = objects.list(None).try_collect::<Vec<_>>().await.unwrap();
    let stored_bytes: u64 = inventory.iter().map(|o| o.size).sum();
    let stored_pack_bytes: u64 = inventory
        .iter()
        .filter(|o| o.location.as_ref().starts_with("packs/"))
        .map(|o| o.size)
        .sum();
    for (cache_label, cache_bytes) in [
        ("disabled", 0),
        ("below-largest-pack", largest - 1),
        ("fits-largest-pack", largest),
        ("above-largest-pack", largest + 1),
        ("fits-working-set", working_bytes),
    ] {
        // Fresh handle for every cache case. Opening the catalog is excluded;
        // lazy manifest/index reads during reconstruction remain in the timing.
        let reader = open(objects.clone(), cache_bytes).await;
        for phase in ["cold", "warm"] {
            // Together with the cold pass, two complete reads also promote
            // packs containing only one referenced chunk (default threshold 2).
            if phase == "warm" {
                audit(&reader, &id, expected).await;
            }
            reader.reset_pack_read_stats();
            let nanos = audit(&reader, &id, expected).await;
            let stats = reader.pack_read_stats().unwrap();
            let pack_requests = stats.chunk_range_requests
                + stats.whole_pack_requests
                + stats.footer_range_requests;
            let pack_read_bytes =
                stats.chunk_range_bytes + stats.whole_pack_bytes + stats.footer_range_bytes;
            if cache_bytes == 0 {
                assert_eq!(stats.whole_pack_requests, 0);
            }
            if cache_label == "fits-working-set" && phase == "warm" {
                assert_eq!(pack_requests, 0, "ample cache must serve warmed packs");
            }
            println!(
                "fragmentation_sample {}",
                json!({
                    "pattern": pattern, "generation": generation, "layout": layout,
                    "phase": phase, "cache": cache_label, "cache_bytes": cache_bytes,
                    "file_bytes": FILE_BYTES, "pack_target_bytes": PACK_BYTES,
                    "avg_chunk_bytes": DEFAULT_AVG_CHUNK_SIZE, "blob": id.to_string(),
                    "chunks": chunks.len(), "referenced_packs": packs.len(), "pack_runs": runs,
                    "referenced_pack_bytes": working_bytes, "largest_pack_bytes": largest,
                    "stored_bytes": stored_bytes, "stored_pack_bytes": stored_pack_bytes,
                    "nanos": nanos, "pack_requests": pack_requests, "pack_read_bytes": pack_read_bytes,
                    "chunk_range_requests": stats.chunk_range_requests, "whole_pack_requests": stats.whole_pack_requests,
                    "cache_hits": stats.cache_hits, "cache_promotions": stats.cache_promotions,
                    "cache_evictions": stats.cache_evictions, "index_requests": stats.index_requests,
                    "index_bytes": stats.index_bytes,
                    "correctness": "exact bytes and independent BLAKE3; all historical versions verified"
                })
            );
        }
    }
}

#[tokio::test]
#[ignore = "release-mode probe; benchmark run pack-fragmentation"]
async fn benchmark_pack_fragmentation() {
    let points: Vec<usize> = std::env::var("CASITA_FRAGMENTATION_GENERATIONS")
        .unwrap_or_else(|_| "0,1,4,16,32".into())
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(points.first() == Some(&0) && points.windows(2).all(|p| p[0] < p[1]));
    assert!(*points.last().unwrap() <= 64);
    for pattern in ["localized", "scattered"] {
        let objects: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let writer = open(objects.clone(), 0).await;
        let mut expected = vec![0; FILE_BYTES];
        random(0, &mut expected);
        let mut versions = Vec::new();
        for generation in 0..=*points.last().unwrap() {
            if generation > 0 {
                let slot = if pattern == "localized" {
                    0
                } else {
                    (generation - 1) * 17 % 64
                };
                let offset = slot * DEFAULT_AVG_CHUNK_SIZE as usize + 8192;
                random(generation as u64, &mut expected[offset..offset + 4096]);
            }
            let id = BlobId::new(blake3::hash(&expected).into());
            assert_eq!(writer.put_slice(&expected).await.unwrap(), id);
            writer.flush().await.unwrap();
            versions.push((id, expected.clone()));
            if points.contains(&generation) {
                // Check every retained version before emitting success evidence.
                let verifier = open(objects.clone(), 0).await;
                for (id, bytes) in &versions {
                    audit(&verifier, id, bytes).await;
                }
                drop(verifier);
                let fresh: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
                let fresh_writer = open(fresh.clone(), 0).await;
                assert_eq!(fresh_writer.put_slice(&expected).await.unwrap(), id);
                fresh_writer.flush().await.unwrap();
                drop(fresh_writer);
                // Alternate layout order to reduce systematic timing bias.
                if generation.is_multiple_of(2) {
                    measure(objects.clone(), &expected, pattern, generation, "history").await;
                    measure(fresh, &expected, pattern, generation, "fresh").await;
                } else {
                    measure(fresh, &expected, pattern, generation, "fresh").await;
                    measure(objects.clone(), &expected, pattern, generation, "history").await;
                }
            }
        }
    }
}
