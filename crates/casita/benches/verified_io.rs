//! Permanent Bao ingestion, first-byte, streaming, and overwrite boundaries.
//! Every sample checks its content identity or authenticated output.
mod bench_util;

use casita::experimental::{BlobId, BlobStore};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::io::AsyncReadExt;

fn verified_io(c: &mut Criterion) {
    let runtime = bench_util::runtime();
    // BLAKE3 leaf, Bao group, CDC minimum, outboard spill, and leaf-CV spill.
    let sizes = [
        1023,
        1025,
        16383,
        16385,
        131071,
        131073,
        16 * 1024 * 1024 + 16384 - 1,
        16 * 1024 * 1024 + 16384 + 1,
        32 * 1024 * 1024 - 1,
        32 * 1024 * 1024 + 1,
    ];
    let mut group = c.benchmark_group("verified_io");
    group.sample_size(10);
    for size in sizes {
        let data = bench_util::random_bytes(91, size);
        let expected = BlobId::new(blake3::hash(&data).into());
        let (store, _objects) = bench_util::memory_store(256 * 1024);
        assert_eq!(runtime.block_on(store.put_slice(&data)).unwrap(), expected);
        assert_eq!(
            runtime
                .block_on(store.read_to_vec(&expected))
                .unwrap()
                .unwrap(),
            data
        );
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(BenchmarkId::new("ingest", size), |b| {
            b.to_async(&runtime).iter(|| async {
                let (fresh, _) = bench_util::memory_store(256 * 1024);
                assert_eq!(fresh.put_slice(&data).await.unwrap(), expected);
            });
        });
        group.bench_function(BenchmarkId::new("stream", size), |b| {
            b.to_async(&runtime).iter(|| async {
                let mut reader = store
                    .open_verified(&expected, size as u64)
                    .await
                    .unwrap()
                    .unwrap();
                let mut hash = blake3::Hasher::new();
                let mut buffer = vec![0; 64 * 1024];
                let mut count = 0;
                loop {
                    let read = reader.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    hash.update(&buffer[..read]);
                    count += read;
                }
                assert_eq!(count, size);
                assert_eq!(BlobId::new(hash.finalize().into()), expected);
            });
        });
        group.throughput(Throughput::Bytes(size.min(1024) as u64));
        group.bench_function(BenchmarkId::new("first_1024_bytes", size), |b| {
            b.to_async(&runtime).iter(|| async {
                let mut reader = store
                    .open_verified(&expected, size as u64)
                    .await
                    .unwrap()
                    .unwrap();
                let mut prefix = vec![0; size.min(1024)];
                reader.read_exact(&mut prefix).await.unwrap();
                assert_eq!(prefix, data[..prefix.len()]);
            });
        });
        for (label, offset) in [
            ("overwrite_within", size / 2),
            (
                "overwrite_across",
                (size / 2 / 16384 * 16384).saturating_sub(100),
            ),
        ] {
            let replacement = vec![42; 300.min(size - offset)];
            let mut changed = data.clone();
            changed[offset..offset + replacement.len()].copy_from_slice(&replacement);
            let new = BlobId::new(blake3::hash(&changed).into());
            let (id, _) = runtime
                .block_on(store.overwrite(&expected, size as u64, offset as u64, &replacement))
                .unwrap();
            assert_eq!(id, new);
            assert_eq!(
                runtime.block_on(store.read_to_vec(&new)).unwrap().unwrap(),
                changed
            );
            group.throughput(Throughput::Bytes(replacement.len() as u64));
            group.bench_function(BenchmarkId::new(label, size), |b| {
                b.to_async(&runtime).iter(|| async {
                    let (id, _) = store
                        .overwrite(&expected, size as u64, offset as u64, &replacement)
                        .await
                        .unwrap();
                    assert_eq!(id, new);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, verified_io);
criterion_main!(benches);
