//! Full manifest reads across leaf, concurrency, and tree-height boundaries.
use casita::experimental::{BlobId, BlobStore, BlobSync, ChunkId, ChunkMeta, ChunkedBlobStore};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use object_store::{
    ObjectStore,
    memory::InMemory,
    path::Path,
    throttle::{ThrottleConfig, ThrottledStore},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn manifest_reads(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("manifest_reads");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(1));
    for delay_ms in [0, 20] {
        for count in [63usize, 64, 65, 511, 512, 513, 4095, 4096, 4097] {
            let origin = Arc::new(InMemory::new());
            let writer = ChunkedBlobStore::new(origin.clone(), Path::default(), 16384);
            // Build genuine verified content before applying latency. Only
            // metadata reconstruction is measured below.
            let (id, chunks) = runtime.block_on(async {
                let mut hasher = blake3::Hasher::new();
                let mut chunks = Vec::new();
                for i in 0..count {
                    let mut bytes = vec![0; 16384 + i];
                    bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
                    hasher.update(&bytes);
                    let meta = ChunkMeta {
                        digest: ChunkId::new(blake3::hash(&bytes).into()),
                        size: bytes.len() as u64,
                    };
                    writer
                        .put_chunk(&meta, zstd::encode_all(bytes.as_slice(), 0).unwrap().into())
                        .await
                        .unwrap();
                    chunks.push(meta);
                }
                let id = BlobId::new(hasher.finalize().into());
                writer.put_manifest(&id, chunks.clone()).await.unwrap();
                (id, chunks)
            });
            let objects: Arc<dyn ObjectStore> = if delay_ms == 0 {
                origin
            } else {
                Arc::new(ThrottledStore::new(
                    origin,
                    ThrottleConfig {
                        wait_get_per_call: Duration::from_millis(delay_ms),
                        ..Default::default()
                    },
                ))
            };
            let store = ChunkedBlobStore::new(objects, Path::default(), 16384);
            group.bench_function(
                BenchmarkId::new(format!("delay_{delay_ms}ms"), count),
                |b| {
                    b.iter_custom(|iterations| {
                        runtime.block_on(async {
                            let mut elapsed = Duration::ZERO;
                            for _ in 0..iterations {
                                let started = Instant::now();
                                let actual = store.chunks(&id).await.unwrap().unwrap();
                                elapsed += started.elapsed();
                                // Exact order, sizes, identities, and multiplicity, outside timing.
                                assert_eq!(actual, chunks);
                            }
                            elapsed
                        })
                    });
                },
            );
        }
    }
    group.finish();
}
criterion_group!(benches, manifest_reads);
criterion_main!(benches);
