//! Slice 1: the import write path.
//!
//! Two layers are measured. First the CPU stages in isolation — FastCDC
//! chunking, BLAKE3 hashing, and zstd compression — so we know which one is the
//! ceiling. Then the composed `write_blob` path end to end, into both an in-memory
//! object store (pure CPU + plumbing, no IO) and a local filesystem store (real
//! per-chunk file creation). Finally two sweeps that justify the defaults: the
//! FastCDC average chunk size and the zstd compression level.
//!
//! Run with:  cargo bench --bench write_path

mod bench_util;

use bench_util::{
    compressible_bytes, local_store, memory_store, random_bytes, runtime, write_blob,
};
use casita::experimental::DEFAULT_AVG_CHUNK_SIZE;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// Hash-input sizes and both sides of the FastCDC minimum/maximum. Each cold
/// write must reproduce the reference chunking as well as the whole-blob hash.
fn hash_write_boundaries(c: &mut Criterion) {
    use casita::experimental::{BlobId, BlobStore, ChunkId, ChunkMeta};
    let rt = runtime();
    let avg = DEFAULT_AVG_CHUNK_SIZE;
    let min = avg / 2;
    let max = avg * 2;
    let mut group = c.benchmark_group("hash_write_boundaries");
    group.sample_size(10);
    for size in [
        0,
        64,
        KIB,
        16 * KIB,
        64 * KIB,
        min as usize - 1,
        min as usize,
        min as usize + 1,
        avg as usize,
        max as usize - 1,
        max as usize,
        max as usize + 1,
        MIB,
        4 * MIB,
    ] {
        for (flavor, bytes) in [("random", random_bytes(83, size)), ("zeros", vec![0; size])] {
            let expected = BlobId::new(blake3::hash(&bytes).into());
            let chunks: Vec<_> =
                fastcdc::v2020::FastCDC::new(&bytes, min as usize, avg as usize, max as usize)
                    .map(|chunk| ChunkMeta {
                        digest: ChunkId::new(
                            blake3::hash(&bytes[chunk.offset..chunk.offset + chunk.length]).into(),
                        ),
                        size: chunk.length as u64,
                    })
                    .collect();
            group.throughput(Throughput::Bytes(size as u64));
            group.bench_function(BenchmarkId::new(flavor, size), |b| {
                b.iter_custom(|iterations| {
                    rt.block_on(async {
                        let mut elapsed = std::time::Duration::ZERO;
                        for _ in 0..iterations {
                            let (store, _) = memory_store(avg);
                            let started = std::time::Instant::now();
                            let digest = write_blob(&store, &bytes).await;
                            elapsed += started.elapsed();
                            assert_eq!(digest, expected);
                            assert_eq!(store.chunks(&digest).await.unwrap().unwrap(), chunks);
                            assert_eq!(store.read_to_vec(&digest).await.unwrap().unwrap(), bytes);
                        }
                        elapsed
                    })
                });
            });
        }
    }
    group.finish();
}

/// FastCDC chunking, BLAKE3 hashing, and zstd (default level) in isolation, over
/// a 16 MiB buffer. Compression is measured on both incompressible and
/// compressible data because zstd's cost differs sharply between them.
fn cpu_stages(c: &mut Criterion) {
    let len = 16 * MIB;
    let rnd = random_bytes(1, len);
    let txt = compressible_bytes(2, len);
    let (min, avg, max) = (
        DEFAULT_AVG_CHUNK_SIZE / 2,
        DEFAULT_AVG_CHUNK_SIZE,
        DEFAULT_AVG_CHUNK_SIZE * 2,
    );

    let mut g = c.benchmark_group("cpu_stages");
    g.throughput(Throughput::Bytes(len as u64));

    g.bench_function("fastcdc_chunk", |b| {
        b.iter(|| {
            let mut n = 0u64;
            for chunk in fastcdc::v2020::FastCDC::new(
                black_box(&rnd),
                min as usize,
                avg as usize,
                max as usize,
            ) {
                n += chunk.length as u64;
            }
            black_box(n)
        })
    });

    g.bench_function("blake3_hash", |b| {
        b.iter(|| black_box(blake3::hash(black_box(&rnd))))
    });

    g.bench_function("zstd_compress_incompressible", |b| {
        b.iter(|| {
            black_box(
                zstd::encode_all(black_box(&rnd[..]), zstd::DEFAULT_COMPRESSION_LEVEL).unwrap(),
            )
        })
    });

    g.bench_function("zstd_compress_compressible", |b| {
        b.iter(|| {
            black_box(
                zstd::encode_all(black_box(&txt[..]), zstd::DEFAULT_COMPRESSION_LEVEL).unwrap(),
            )
        })
    });

    let compressed = zstd::encode_all(&txt[..], zstd::DEFAULT_COMPRESSION_LEVEL).unwrap();
    g.bench_function("zstd_decompress_compressible", |b| {
        b.iter(|| black_box(zstd::decode_all(black_box(&compressed[..])).unwrap()))
    });

    g.finish();
}

/// End-to-end `write_blob` into a fresh in-memory store (every iteration is a cold
/// write: fresh store, empty chunk index). Covers the single-chunk fast path
/// (4 KiB), a small multi-chunk blob (1 MiB), and a large one (16 MiB), each in
/// incompressible and compressible flavors.
fn put_blob_memory(c: &mut Criterion) {
    let rt = runtime();
    let mut g = c.benchmark_group("put_blob_memory");
    g.sample_size(20);

    for &len in &[4 * KIB, MIB, 16 * MIB] {
        for (flavor, data) in [
            ("random", random_bytes(3, len)),
            ("compressible", compressible_bytes(4, len)),
        ] {
            g.throughput(Throughput::Bytes(len as u64));
            g.bench_with_input(BenchmarkId::new(flavor, human(len)), &data, |b, data| {
                b.to_async(&rt).iter_batched(
                    || memory_store(DEFAULT_AVG_CHUNK_SIZE).0,
                    |store| async move { black_box(write_blob(&store, data).await) },
                    BatchSize::SmallInput,
                )
            });
        }
    }
    g.finish();
}

/// End-to-end `write_blob` into a fresh local-filesystem store, so per-chunk file
/// creation and the object_store local backend are on the clock. Incompressible
/// data only — this bench is about IO, not compression.
fn put_blob_local_fs(c: &mut Criterion) {
    let rt = runtime();
    let mut g = c.benchmark_group("put_blob_local_fs");
    g.sample_size(10);

    for &len in &[MIB, 16 * MIB] {
        let data = random_bytes(5, len);
        g.throughput(Throughput::Bytes(len as u64));
        g.bench_with_input(BenchmarkId::from_parameter(human(len)), &data, |b, data| {
            b.to_async(&rt).iter_batched(
                || local_store(DEFAULT_AVG_CHUNK_SIZE),
                |(store, dir)| async move { (black_box(write_blob(&store, data).await), store, dir) },
                BatchSize::SmallInput,
            )
        });
    }
    g.finish();
}

/// Sweep the FastCDC average chunk size (64 KiB / 256 KiB / 1 MiB) for the same
/// 16 MiB blob, into memory. Smaller chunks mean more BLAKE3 calls, more zstd
/// frames, and a longer manifest; this quantifies that trade against the
/// 256 KiB default.
fn chunk_size_sweep(c: &mut Criterion) {
    let rt = runtime();
    let len = 16 * MIB;
    let data = random_bytes(6, len);

    let mut g = c.benchmark_group("chunk_size_sweep");
    g.sample_size(20);
    g.throughput(Throughput::Bytes(len as u64));

    for &avg in &[64 * KIB as u32, 256 * KIB as u32, MIB as u32] {
        g.bench_with_input(
            BenchmarkId::from_parameter(human(avg as usize)),
            &data,
            |b, data| {
                b.to_async(&rt).iter_batched(
                    || memory_store(avg).0,
                    |store| async move { black_box(write_blob(&store, data).await) },
                    BatchSize::SmallInput,
                )
            },
        );
    }
    g.finish();
}

/// Sweep the zstd compression level (1 / 3=default / 9 / 19) over a 16 MiB
/// compressible buffer, measuring the compressor alone. This is the evidence
/// behind the choice of `zstd::DEFAULT_COMPRESSION_LEVEL`.
fn zstd_level_sweep(c: &mut Criterion) {
    // Deliberately small: the high levels are orders of magnitude slower, so a
    // 4 MiB buffer keeps the sweep bounded while still exposing the cliff.
    let len = 4 * MIB;
    let data = compressible_bytes(7, len);

    let mut g = c.benchmark_group("zstd_level_sweep");
    g.sample_size(10);
    g.throughput(Throughput::Bytes(len as u64));

    for &level in &[1, zstd::DEFAULT_COMPRESSION_LEVEL, 9, 19] {
        let compressed = zstd::bulk::compress(&data, level).unwrap();
        assert_eq!(
            zstd::bulk::decompress(&compressed, data.len()).unwrap(),
            data
        );
        eprintln!(
            "compression-ratio level={level} input_bytes={} compressed_bytes={} ratio={}",
            data.len(),
            compressed.len(),
            compressed.len() as f64 / data.len() as f64
        );
        g.bench_with_input(BenchmarkId::from_parameter(level), &data, |b, data| {
            b.iter(|| black_box(zstd::bulk::compress(black_box(&data[..]), level).unwrap()))
        });
    }
    g.finish();
}

fn packed_writes(c: &mut Criterion) {
    use casita::experimental::{BlobStore, ChunkedBlobStore};
    use object_store::{memory::InMemory, path::Path};
    use std::sync::Arc;
    let rt = runtime();
    let mut group = c.benchmark_group("packed_write");
    group.sample_size(10);
    for length in [4 * KIB, MIB, 16 * MIB] {
        let bytes = random_bytes(71, length);
        group.throughput(Throughput::Bytes(length as u64));
        group.bench_function(human(length), |b| {
            b.iter_custom(|iterations| {
                rt.block_on(async {
                    let mut elapsed = std::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let store = ChunkedBlobStore::packed_with_options(
                            Arc::new(InMemory::new()),
                            Path::default(),
                            DEFAULT_AVG_CHUNK_SIZE,
                            casita::experimental::PackOptions {
                                target_size: 4 * MIB as u64,
                                cache_capacity: 0,
                            },
                        )
                        .await
                        .unwrap();
                        let started = std::time::Instant::now();
                        let digest = write_blob(&store, &bytes).await;
                        store.flush().await.unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(store.read_to_vec(&digest).await.unwrap().unwrap(), bytes);
                    }
                    elapsed
                })
            });
        });
    }
    group.finish();
}

fn canonical_directory(c: &mut Criterion) {
    use casita::experimental::{BlobId, Digest, Directory, Node, PathComponent};
    let mut group = c.benchmark_group("canonical_directory");
    for count in [16usize, 1024, 16384] {
        let directory = Directory::try_from_iter((0..count).map(|index| {
            (
                PathComponent::try_from(format!("file-{index:08}").as_str()).unwrap(),
                Node::File {
                    digest: BlobId::new(Digest::hash(&index.to_le_bytes())),
                    size: 8,
                    executable: false,
                },
            )
        }))
        .unwrap();
        let encoded = directory.encode();
        assert_eq!(Directory::decode(&encoded).unwrap(), directory);
        group.bench_function(format!("encode/{count}"), |b| {
            b.iter(|| black_box(directory.encode()))
        });
        group.bench_function(format!("decode/{count}"), |b| {
            b.iter(|| black_box(Directory::decode(&encoded).unwrap()))
        });
        let name = format!("file-{:08}", count / 2);
        assert!(directory.get(&name).is_some());
        group.bench_function(format!("lookup/{count}"), |b| {
            b.iter(|| black_box(directory.get(&name)))
        });
    }
    group.finish();
}

/// A compact human label for a byte size, for stable benchmark ids.
fn human(bytes: usize) -> String {
    if bytes >= MIB {
        format!("{}MiB", bytes / MIB)
    } else {
        format!("{}KiB", bytes / KIB)
    }
}

criterion_group!(
    benches,
    cpu_stages,
    hash_write_boundaries,
    put_blob_memory,
    put_blob_local_fs,
    chunk_size_sweep,
    zstd_level_sweep,
    packed_writes,
    canonical_directory,
);
criterion_main!(benches);
