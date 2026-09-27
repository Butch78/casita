//! Verified-read and repair costs for [`RepairingBlobStore`].
//!
//! Every destructive scenario prepares a fresh near tier outside its timed
//! interval. The measured operation therefore starts with the intended damage
//! already present and ends only after the returned bytes have been validated.
//!
//! Run with: `cargo bench --bench repairing`

mod bench_util;

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_util::{memory_store, random_bytes, runtime, write_blob};
use bytes::Bytes;
use casita::experimental::DEFAULT_AVG_CHUNK_SIZE;
use casita::experimental::{BlobId, BlobStore, ChunkedBlobStore, RepairingBlobStore};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use object_store::{ObjectStoreExt, path::Path};
use tokio::sync::Barrier;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const PAYLOAD_LEN: usize = 8 * MIB;
const RANGE_LEN: u64 = 64 * KIB as u64;
const RANGE_OFFSET: u64 = 3 * MIB as u64 + 17;
const CONCURRENT_READERS: usize = 8;

/// Healthy full reads expose the adapter's deliberate complete-validation
/// pass. Healthy Bao reads show the smaller coordination overhead when the
/// near tier can authenticate the requested range directly.
fn healthy_reads(c: &mut Criterion) {
    let rt = runtime();
    let data = random_bytes(101, PAYLOAD_LEN);
    let (near, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let (far, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let digest = rt.block_on(async {
        let digest = write_blob(&near, &data).await;
        assert_eq!(write_blob(&far, &data).await, digest);
        digest
    });
    let repairing = RepairingBlobStore::new(near.clone(), far);

    let mut group = c.benchmark_group("repairing_healthy_reads");
    group.sample_size(10);

    group.throughput(Throughput::Bytes(PAYLOAD_LEN as u64));
    group.bench_function("chunked_full", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(near.read_to_vec(&digest).await.unwrap().unwrap()) })
    });
    group.bench_function("repairing_full", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(repairing.read_to_vec(&digest).await.unwrap().unwrap()) })
    });

    group.throughput(Throughput::Bytes(RANGE_LEN));
    group.bench_function("chunked_bao_range", |b| {
        b.to_async(&rt).iter(|| async {
            black_box(
                near.verified_read(&digest, RANGE_OFFSET, RANGE_LEN)
                    .await
                    .unwrap(),
            )
        })
    });
    group.bench_function("repairing_bao_range", |b| {
        b.to_async(&rt).iter(|| async {
            black_box(
                repairing
                    .verified_read(&digest, RANGE_OFFSET, RANGE_LEN)
                    .await
                    .unwrap(),
            )
        })
    });
    group.finish();
}

fn healthy_size_sweep(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("repairing_size_sweep");
    group.sample_size(10);
    for length in [4 * KIB, MIB, 32 * MIB] {
        let data = random_bytes(105, length);
        let (near, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
        let (far, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
        let digest = rt.block_on(write_blob(&near, &data));
        let repairing = RepairingBlobStore::new(near.clone(), far);
        assert_eq!(
            rt.block_on(repairing.read_to_vec(&digest))
                .unwrap()
                .unwrap(),
            data
        );
        group.throughput(Throughput::Bytes(length as u64));
        group.bench_function(format!("chunked/{length}"), |b| {
            b.to_async(&rt)
                .iter(|| async { black_box(near.read_to_vec(&digest).await.unwrap().unwrap()) })
        });
        group.bench_function(format!("repairing/{length}"), |b| {
            b.to_async(&rt).iter(|| async {
                black_box(repairing.read_to_vec(&digest).await.unwrap().unwrap())
            })
        });
    }
    group.finish();
}

/// Replace a corrupt compressed chunk from an independently verified far tier.
/// Fresh near stores and fault injection are setup work, not part of the
/// reported duration.
fn corrupt_chunk_repair(c: &mut Criterion) {
    let rt = runtime();
    let data = random_bytes(102, PAYLOAD_LEN);
    let (far, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let digest = rt.block_on(write_blob(&far, &data));

    let mut group = c.benchmark_group("repairing_corrupt_chunk");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(PAYLOAD_LEN as u64));
    group.bench_function(BenchmarkId::from_parameter("8MiB"), |b| {
        b.iter_custom(|iterations| {
            rt.block_on(async {
                let mut measured = Duration::ZERO;
                for _ in 0..iterations {
                    let repairing = corrupt_chunk_fixture(&data, digest, &far).await;
                    let started = Instant::now();
                    let bytes = repairing.read_to_vec(&digest).await.unwrap().unwrap();
                    measured += started.elapsed();
                    assert_eq!(bytes, data);
                }
                measured
            })
        })
    });
    group.finish();
}

/// Recompute corrupt Bao state from verified local payload bytes, then retry
/// the requested range. A fresh damaged outboard is installed per iteration.
fn bao_rebuild(c: &mut Criterion) {
    let rt = runtime();
    let data = random_bytes(103, PAYLOAD_LEN);
    let (far, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let digest = rt.block_on(write_blob(&far, &data));
    let expected = &data[RANGE_OFFSET as usize..(RANGE_OFFSET + RANGE_LEN) as usize];

    let mut group = c.benchmark_group("repairing_bao_rebuild");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(PAYLOAD_LEN as u64));
    group.bench_function(BenchmarkId::from_parameter("8MiB"), |b| {
        b.iter_custom(|iterations| {
            rt.block_on(async {
                let mut measured = Duration::ZERO;
                for _ in 0..iterations {
                    let repairing = corrupt_outboard_fixture(&data, digest, &far).await;
                    let started = Instant::now();
                    let bytes = repairing
                        .verified_read(&digest, RANGE_OFFSET, RANGE_LEN)
                        .await
                        .unwrap();
                    measured += started.elapsed();
                    assert_eq!(bytes.as_ref(), expected);
                }
                measured
            })
        })
    });
    group.finish();
}

/// Release several readers onto one corrupt blob together. The result captures
/// the latency of the adapter's single-flight repair plus delivery to every
/// waiter, while setup remains outside the timer.
fn concurrent_repair(c: &mut Criterion) {
    let rt = runtime();
    let data = random_bytes(104, PAYLOAD_LEN);
    let (far, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let digest = rt.block_on(write_blob(&far, &data));

    let mut group = c.benchmark_group("repairing_concurrent_readers");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((PAYLOAD_LEN * CONCURRENT_READERS) as u64));
    group.bench_function(BenchmarkId::from_parameter(CONCURRENT_READERS), |b| {
        b.iter_custom(|iterations| {
            rt.block_on(async {
                let mut measured = Duration::ZERO;
                for _ in 0..iterations {
                    let repairing = corrupt_chunk_fixture(&data, digest, &far).await;
                    let barrier = Arc::new(Barrier::new(CONCURRENT_READERS + 1));
                    let mut readers = Vec::with_capacity(CONCURRENT_READERS);
                    for _ in 0..CONCURRENT_READERS {
                        let repairing = repairing.clone();
                        let barrier = barrier.clone();
                        readers.push(tokio::spawn(async move {
                            barrier.wait().await;
                            repairing.read_to_vec(&digest).await
                        }));
                    }

                    let started = Instant::now();
                    barrier.wait().await;
                    let mut outputs = Vec::with_capacity(CONCURRENT_READERS);
                    for reader in readers {
                        outputs.push(reader.await.unwrap().unwrap().unwrap());
                    }
                    measured += started.elapsed();
                    assert!(outputs.iter().all(|bytes| bytes == &data));
                }
                measured
            })
        })
    });
    group.finish();
}

async fn corrupt_chunk_fixture(
    data: &[u8],
    digest: BlobId,
    far: &ChunkedBlobStore,
) -> RepairingBlobStore {
    let (near, backend) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    assert_eq!(write_blob(&near, data).await, digest);
    let chunk = near.chunks(&digest).await.unwrap().unwrap()[0].digest;
    let hex = chunk.digest().to_hex();
    let path = Path::from(format!("chunks/b3/{}/{}", &hex[..2], hex));
    backend
        .put(&path, Bytes::from_static(b"corrupt zstd frame").into())
        .await
        .unwrap();
    RepairingBlobStore::new(near, far.clone())
}

async fn corrupt_outboard_fixture(
    data: &[u8],
    digest: BlobId,
    far: &ChunkedBlobStore,
) -> RepairingBlobStore {
    let (near, _) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
    assert_eq!(write_blob(&near, data).await, digest);
    near.put_outboard(&digest, Bytes::from_static(b"corrupt bao outboard"))
        .await
        .unwrap();
    RepairingBlobStore::new(near, far.clone())
}

criterion_group!(
    benches,
    healthy_reads,
    healthy_size_sweep,
    corrupt_chunk_repair,
    bao_rebuild,
    concurrent_repair,
);
criterion_main!(benches);
