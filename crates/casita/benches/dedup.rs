//! Slice 2: deduplication throughput.
//!
//! Re-importing content casita already has should be cheap, but *how* cheap
//! depends on the chunk index. All three variants below re-import the same
//! 16 MiB blob; the difference is the state of the store:
//!
//! - `full_write`  — a fresh, empty store: every chunk is hashed, zstd-compressed
//!   and uploaded. The baseline cost of a novel blob.
//! - `warm_index`  — the same store, already populated: the in-memory chunk index
//!   answers "present?" with no round-trip, so writes skip upload + compression.
//! - `cold_index`  — a fresh store over a backend that already holds the chunks:
//!   the index is empty, so every chunk costs a `HEAD` before it is skipped.
//!
//! `warm_index` vs `cold_index` is the value of the in-process `ChunkIndex`;
//! both vs `full_write` is the value of dedup at all.
//!
//! Run with:  cargo bench --bench dedup

mod bench_util;

use bench_util::{memory_store, mixed_corpus, random_bytes, runtime, store_over, write_blob};
use casita::experimental::BlobStore;
use casita::experimental::DEFAULT_AVG_CHUNK_SIZE;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use std::collections::HashSet;
use std::hint::black_box;

const MIB: usize = 1024 * 1024;

fn rewrite(c: &mut Criterion) {
    let rt = runtime();
    let len = 16 * MIB;
    let buf = random_bytes(42, len);
    let data: &[u8] = &buf; // borrowed so it can be re-imported every iteration.

    let mut g = c.benchmark_group("rewrite");
    g.throughput(Throughput::Bytes(len as u64));
    g.sample_size(20);

    // Baseline: a novel blob into an empty store, every chunk uploaded.
    g.bench_function("full_write", |b| {
        b.to_async(&rt).iter_batched(
            || memory_store(DEFAULT_AVG_CHUNK_SIZE).0,
            |store| async move { black_box(write_blob(&store, data).await) },
            BatchSize::SmallInput,
        )
    });

    // Warm index: populate outside timing, including the first iteration.
    g.bench_function("warm_index", |b| {
        let store = memory_store(DEFAULT_AVG_CHUNK_SIZE).0;
        rt.block_on(write_blob(&store, data));
        b.to_async(&rt).iter(|| {
            let store = &store;
            async move { black_box(write_blob(store, data).await) }
        })
    });

    // Cold index: populate a backend once (outside timing), then on each
    // iteration open a fresh store over it. The index is empty, so presence is
    // confirmed by a HEAD per chunk; the writes are idempotent so the backend
    // never grows.
    let prefilled = {
        let (store, backend) = memory_store(DEFAULT_AVG_CHUNK_SIZE);
        rt.block_on(write_blob(&store, data));
        backend
    };
    g.bench_function("cold_index", |b| {
        b.to_async(&rt).iter_batched(
            || store_over(prefilled.clone(), DEFAULT_AVG_CHUNK_SIZE),
            |store| async move { black_box(write_blob(&store, data).await) },
            BatchSize::SmallInput,
        )
    });

    g.finish();
}

/// Write a representative mixed corpus and then a locally edited version into
/// the same fresh store. These workloads exercise FastCDC boundary stability;
/// exact re-import alone cannot reveal how much unchanged content survives an
/// insertion, deletion, or replacement.
fn edited_reimport(c: &mut Criterion) {
    let rt = runtime();
    let len = 8 * MIB;
    let base = mixed_corpus(91, len);
    let edit_len = 8 * 1024;

    let mut inserted = base.clone();
    inserted.splice(len / 3..len / 3, random_bytes(92, edit_len));

    let mut deleted = base.clone();
    deleted.drain(2 * len / 3..2 * len / 3 + edit_len);

    let mut replaced = base.clone();
    replaced[len / 2..len / 2 + edit_len].copy_from_slice(&random_bytes(93, edit_len));

    let edits = [
        ("insert_8KiB", inserted),
        ("delete_8KiB", deleted),
        ("replace_8KiB", replaced),
    ];
    for (mutation, edited) in &edits {
        let (shared_bytes, edited_bytes, base_chunks, edited_chunks) =
            rt.block_on(chunk_overlap(&base, edited));
        eprintln!(
            "dedup-overlap {mutation}: {shared_bytes}/{edited_bytes} edited bytes in shared chunks; \
             chunks {base_chunks}->{edited_chunks}"
        );
    }

    let mut group = c.benchmark_group("base_and_edited_reimport");
    group.sample_size(10);
    for (mutation, edited) in &edits {
        group.throughput(Throughput::Bytes((base.len() + edited.len()) as u64));
        group.bench_with_input(*mutation, &edited, |b, edited| {
            b.to_async(&rt).iter_batched(
                || memory_store(DEFAULT_AVG_CHUNK_SIZE).0,
                |store| {
                    let base = base.as_slice();
                    let edited = edited.as_slice();
                    async move {
                        black_box(write_blob(&store, base).await);
                        black_box(write_blob(&store, edited).await)
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
    let mut group = c.benchmark_group("edited_only");
    group.sample_size(10);
    for (mutation, edited) in &edits {
        group.throughput(Throughput::Bytes(edited.len() as u64));
        group.bench_function(*mutation, |b| {
            b.iter_custom(|iterations| {
                rt.block_on(async {
                    let mut elapsed = std::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let store = memory_store(DEFAULT_AVG_CHUNK_SIZE).0;
                        write_blob(&store, &base).await;
                        let started = std::time::Instant::now();
                        black_box(write_blob(&store, edited).await);
                        elapsed += started.elapsed();
                    }
                    elapsed
                })
            });
        });
    }
    group.finish();
}

async fn chunk_overlap(base: &[u8], edited: &[u8]) -> (u64, u64, usize, usize) {
    let store = memory_store(DEFAULT_AVG_CHUNK_SIZE).0;
    let base_id = write_blob(&store, base).await;
    let base_chunks = store.chunks(&base_id).await.unwrap().unwrap();
    let base_digests: HashSet<_> = base_chunks.iter().map(|chunk| chunk.digest).collect();
    let edited_id = write_blob(&store, edited).await;
    let edited_chunks = store.chunks(&edited_id).await.unwrap().unwrap();
    let shared_bytes = edited_chunks
        .iter()
        .filter(|chunk| base_digests.contains(&chunk.digest))
        .map(|chunk| chunk.size)
        .sum();
    (
        shared_bytes,
        edited.len() as u64,
        base_chunks.len(),
        edited_chunks.len(),
    )
}

criterion_group!(benches, rewrite, edited_reimport);
criterion_main!(benches);
