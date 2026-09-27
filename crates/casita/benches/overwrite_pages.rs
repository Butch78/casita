//! Fresh edits across both sides of metadata page and tree-height thresholds.
//! Timing excludes fixtures, full rehash/readback gates, and orphan cleanup.
mod bench_util;
use bench_util::io_counter::{Counted, Counts};
use casita::experimental::{
    BlobGc, BlobId, BlobStore, BlobSync, ChunkId, ChunkMeta, ChunkedBlobStore,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use object_store::{memory::InMemory, path::Path};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// Count transient Rust heap above the quiescent fixture baseline. This dedicated
// binary owns its runtime; timings include allocator-accounting overhead.
struct MeasuredAllocator;
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
fn added(size: usize) {
    let live = LIVE.fetch_add(size as u64, Ordering::Relaxed) + size as u64;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            added(layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(ptr, layout);
        }
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let ptr = unsafe { System.realloc(ptr, layout, size) };
        if !ptr.is_null() {
            LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
            added(size);
        }
        ptr
    }
}
#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

fn overwrite_pages(c: &mut Criterion) {
    let runtime = bench_util::runtime();
    let mut group = c.benchmark_group("overwrite_pages");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(1));
    // Fixed 16 KiB storage chunks make both metadata thresholds exact:
    // 64 chunk rows per leaf; 64 Bao pairs per byte leaf; fanout 64.
    for count in [63usize, 64, 65, 66, 4095, 4096, 4097, 4098, 4099, 16384] {
        let size = count * 16384;
        let original = bench_util::random_bytes(91, size);
        let old = BlobId::new(blake3::hash(&original).into());
        let counts = Arc::new(Counts::default());
        let objects = Arc::new(Counted {
            inner: Arc::new(InMemory::new()),
            counts: counts.clone(),
        });
        let store = ChunkedBlobStore::new(objects, Path::default(), 16384);
        let chunks = runtime.block_on(async {
            let mut chunks = Vec::new();
            for bytes in original.chunks(16384) {
                let meta = ChunkMeta {
                    digest: ChunkId::new(blake3::hash(bytes).into()),
                    size: bytes.len() as u64,
                };
                store
                    .put_chunk(&meta, zstd::encode_all(bytes, 0).unwrap().into())
                    .await
                    .unwrap();
                chunks.push(meta);
            }
            store.put_manifest(&old, chunks.clone()).await.unwrap();
            assert_eq!(store.read_to_vec(&old).await.unwrap().unwrap(), original);
            chunks
        });
        for (label, offset) in [
            ("within", size / 2 / 16384 * 16384 + 100),
            ("across", size / 2 / 16384 * 16384 - 100),
        ] {
            let mut serial = 0u64;
            let mut maxima = [0u64; 5];
            group.throughput(Throughput::Bytes(300));
            group.bench_function(BenchmarkId::new(label, count), |b| {
                b.iter_custom(|iterations| {
                    runtime.block_on(async {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            serial += 1;
                            let mut replacement = [42; 300];
                            replacement[..8].copy_from_slice(&serial.to_le_bytes());
                            let mut expected = original.clone();
                            expected[offset..offset + replacement.len()]
                                .copy_from_slice(&replacement);
                            let id = BlobId::new(blake3::hash(&expected).into());
                            counts.reset();
                            let baseline = LIVE.load(Ordering::Relaxed);
                            PEAK.store(baseline, Ordering::Relaxed);
                            let start = Instant::now();
                            let (actual, _) = store
                                .overwrite(&old, size as u64, offset as u64, &replacement)
                                .await
                                .unwrap();
                            elapsed += start.elapsed();
                            let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
                            let io = counts.values();
                            for (maximum, value) in
                                maxima.iter_mut().zip(io.into_iter().chain([peak]))
                            {
                                *maximum = (*maximum).max(value);
                            }
                            assert_eq!(actual, id);
                            // File size must not turn a 300-byte edit into a flat
                            // metadata copy, full payload pass, or large allocation.
                            assert!(io[0] < 256 * 1024, "payload reads: {io:?}");
                            assert!(io[1] < 192 * 1024, "metadata reads: {io:?}");
                            assert!(io[3] < 128 * 1024, "metadata writes: {io:?}");
                            assert!(peak < 1024 * 1024, "temporary Rust heap: {peak}");
                            assert_eq!(
                                store.read_to_vec(&actual).await.unwrap().unwrap(),
                                expected
                            );
                            // Remove fresh output outside timing to prevent storage
                            // growth from becoming the benchmark's memory metric.
                            let changed = store.chunks(&actual).await.unwrap().unwrap();
                            store.delete_blob(&actual).await.unwrap();
                            for index in offset / 16384..=(offset + 299) / 16384 {
                                if changed[index].digest != chunks[index].digest {
                                    store.delete_chunk(&changed[index].digest).await.unwrap();
                                }
                            }
                            store.reclaim_metadata().await.unwrap();
                        }
                        elapsed
                    })
                });
            });
            eprintln!(
                "overwrite_pages/{label}/{count} max_payload_read={} max_metadata_read={} max_payload_write={} max_metadata_write={} max_extra_rust_heap={}",
                maxima[0], maxima[1], maxima[2], maxima[3], maxima[4]
            );
        }
    }
    group.finish();
}
criterion_group!(benches, overwrite_pages);
criterion_main!(benches);
