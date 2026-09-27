//! Controlled comparisons for the September 2026 optimization experiments.
//! Each codec variant round-trips the same bytes before it is timed.

mod bench_util;
#[path = "../src/compression.rs"]
#[allow(dead_code, unused_imports)]
mod compression;

use std::hint::black_box;

use casita::experimental::{MetadataStore, ObjectKey, Repository};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// The former chunk decoder is retained as a comparison for the shared codec.
fn streaming_chunk_decode(input: &[u8], limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    zstd::Decoder::new(input)?
        .take(limit as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() > limit {
        return Err(std::io::Error::other(
            "decompressed chunk exceeds size limit",
        ));
    }
    Ok(data)
}

fn chunk_decompression(c: &mut Criterion) {
    let mut group = c.benchmark_group("chunk_decompression");
    group.sample_size(10);
    for size in [
        1024, 65535, 65536, 65537, 131071, 131072, 131073, 262144, 524288,
    ] {
        for (flavor, data) in [
            ("random", bench_util::random_bytes(127, size)),
            ("text", bench_util::compressible_bytes(127, size)),
        ] {
            let sized = compression::compress(&data, zstd::DEFAULT_COMPRESSION_LEVEL).unwrap();
            let unsized_frame =
                zstd::encode_all(data.as_slice(), zstd::DEFAULT_COMPRESSION_LEVEL).unwrap();
            let split = size / 2;
            let mut concatenated = compression::compress(&data[..split], 3).unwrap();
            concatenated.extend(compression::compress(&data[split..], 3).unwrap());
            for (framing, frame) in [
                ("sized", sized),
                ("unsized", unsized_frame),
                ("concatenated", concatenated),
            ] {
                group.throughput(Throughput::Bytes(size as u64));
                let mut variants = [
                    (
                        "streaming",
                        streaming_chunk_decode as fn(&[u8], usize) -> std::io::Result<Vec<u8>>,
                    ),
                    ("reused", compression::decompress),
                ];
                if std::env::var_os("CASITA_CHUNK_DECODE_REVERSE").is_some() {
                    variants.reverse();
                }
                for (variant, decode) in variants {
                    let decoded = decode(&frame, size).unwrap();
                    assert_eq!(decoded, data);
                    assert!(decode(&frame, size - 1).is_err());
                    assert!(decode(&frame[..frame.len() - 1], size).is_err());
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "schema": "casita.chunk-decompression.v1", "variant": variant,
                            "size": size, "flavor": flavor, "framing": framing,
                            "output_capacity_bytes": decoded.capacity(), "correctness": "passed"
                        })
                    );
                    group.bench_function(
                        BenchmarkId::new(format!("{framing}/{flavor}/{size}"), variant),
                        |b| {
                            b.iter_custom(|iterations| {
                                let mut elapsed = std::time::Duration::ZERO;
                                for _ in 0..iterations {
                                    let start = std::time::Instant::now();
                                    let decoded = decode(black_box(&frame), size).unwrap();
                                    elapsed += start.elapsed();
                                    assert_eq!(decoded, data);
                                    black_box(decoded);
                                }
                                elapsed
                            });
                        },
                    );
                }
            }
        }
    }
    group.finish();
}

/// Scratch allocation for known and unknown payload lengths, including both
/// sides of the default 64 KiB read-buffer cap. Every sample verifies EOF,
/// content identity, and the complete payload size.
fn verification_buffers(c: &mut Criterion) {
    use casita::experimental::{
        BlobFormat, BlobId, FormatLimits, ObjectFormat, PayloadReader, VerificationContext,
    };

    struct Reader<'a> {
        bytes: &'a [u8],
        length: Option<u64>,
        largest_buffer: usize,
        eof: bool,
    }

    #[async_trait::async_trait]
    impl PayloadReader for Reader<'_> {
        fn exact_len(&self) -> Option<u64> {
            self.length
        }

        async fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            assert!(!buffer.is_empty());
            self.largest_buffer = self.largest_buffer.max(buffer.len());
            let count = std::io::Read::read(&mut self.bytes, buffer)?;
            self.eof |= count == 0;
            Ok(count)
        }
    }

    let runtime = bench_util::runtime();
    let format = BlobFormat::default();
    let limits = FormatLimits::default();
    let mut group = c.benchmark_group("verification_buffers");
    group.sample_size(10);
    for size in [0, 1, 1024, 16384, 65535, 65536, 65537, 262144] {
        let data = bench_util::random_bytes(103, size);
        let key = ObjectKey::blob(BlobId::new(blake3::hash(&data).into()));
        group.throughput(Throughput::Bytes(size as u64));
        for (kind, length) in [("known", Some(size as u64)), ("unknown", None)] {
            let verify = || async {
                let mut reader = Reader {
                    bytes: &data,
                    length,
                    largest_buffer: 0,
                    eof: false,
                };
                let verified = format
                    .verify(VerificationContext::new(&key, &mut reader), &limits)
                    .await
                    .unwrap();
                assert_eq!(verified.record().key(), &key);
                assert_eq!(verified.record().payload_size(), size as u64);
                assert!(reader.eof);
                assert!(reader.largest_buffer <= limits.read_buffer_bytes);
                reader.largest_buffer
            };
            let largest_buffer = runtime.block_on(verify());
            eprintln!(
                "{}",
                serde_json::json!({"case": format!("verification_buffers/{kind}/{size}"),
                    "largest_read_buffer_bytes": largest_buffer, "correctness": "passed"})
            );
            group.bench_function(BenchmarkId::new(kind, size), |b| {
                b.to_async(&runtime).iter(verify)
            });
        }
    }
    group.finish();
}

fn codecs(c: &mut Criterion) {
    let mut group = c.benchmark_group("record_decode");
    group.sample_size(20);
    for len in [128, 4096, 262_144] {
        let data = bench_util::compressible_bytes(73, len);
        let compressed = zstd::bulk::compress(&data, 1).unwrap();
        let size = zstd::zstd_safe::get_frame_content_size(&compressed)
            .unwrap()
            .unwrap() as usize;
        assert_eq!(size, data.len());
        let mut decoder = zstd::bulk::Decompressor::new().unwrap();
        assert_eq!(decoder.decompress(&compressed, size).unwrap(), data);
        group.throughput(Throughput::Bytes(len as u64));
        group.bench_with_input(
            BenchmarkId::new("limit_capacity", len),
            &compressed,
            |b, input| {
                b.iter(|| {
                    black_box(zstd::bulk::decompress(black_box(input), 256 * 1024 * 1024).unwrap())
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("frame_capacity", len),
            &compressed,
            |b, input| {
                b.iter(|| {
                    let size = zstd::zstd_safe::get_frame_content_size(input)
                        .unwrap()
                        .unwrap() as usize;
                    black_box(zstd::bulk::decompress(black_box(input), size).unwrap())
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("reused_context", len),
            &compressed,
            |b, input| b.iter(|| black_box(decoder.decompress(black_box(input), size).unwrap())),
        );
    }
    group.finish();

    let mut group = c.benchmark_group("chunk_compression");
    group.sample_size(20);
    for len in [4096, 262_144] {
        for (kind, data) in [
            ("text", bench_util::compressible_bytes(73, len)),
            ("random", bench_util::random_bytes(73, len)),
        ] {
            let mut compressor = zstd::bulk::Compressor::new(3).unwrap();
            let compressed = compressor.compress(&data).unwrap();
            assert_eq!(zstd::bulk::decompress(&compressed, len).unwrap(), data);
            group.throughput(Throughput::Bytes(len as u64));
            group.bench_function(format!("fresh/{kind}/{len}"), |b| {
                b.iter(|| black_box(zstd::bulk::compress(black_box(&data), 3).unwrap()))
            });
            group.bench_function(format!("reused/{kind}/{len}"), |b| {
                b.iter(|| black_box(compressor.compress(black_box(&data)).unwrap()))
            });
        }
    }
    group.finish();
}

fn repository_metadata(c: &mut Criterion) {
    let rt = bench_util::runtime();
    let temp = tempfile::tempdir().unwrap();
    let repo = rt.block_on(Repository::local(temp.path())).unwrap();
    let keys = rt.block_on(async {
        let mutation = repo.mutation_session().await.unwrap();
        let mut staged = Vec::new();
        let mut keys = Vec::new();
        for number in 0u64..1024 {
            let object = mutation.stage_blob(&number.to_le_bytes()).await.unwrap();
            keys.push(object.record().key().clone());
            staged.push(object);
        }
        mutation.publish_unrooted(staged).await.unwrap();
        keys
    });
    let snapshot = rt.block_on(repo.metadata().snapshot()).unwrap();
    let mut group = c.benchmark_group("state_lookup");
    group.sample_size(20);
    group.throughput(Throughput::Elements(keys.len() as u64));
    group.bench_function("records_1024", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(snapshot.object_batch(&keys).await.unwrap()) })
    });
    group.bench_function("payloads_1024", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(snapshot.object_payload_batch(&keys).await.unwrap()) })
    });
    group.finish();

    let bytes = bench_util::compressible_bytes(75, 4096);
    let key = ObjectKey::blob(casita::experimental::BlobId::new(
        blake3::hash(&bytes).into(),
    ));
    let mutation = rt.block_on(repo.mutation_session()).unwrap();
    let mut group = c.benchmark_group("generic_stage");
    group.sample_size(20);
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("slice_4k", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(mutation.stage_object(key.clone(), &bytes).await.unwrap()) })
    });
    group.finish();
}

criterion_group!(
    benches,
    codecs,
    repository_metadata,
    verification_buffers,
    chunk_decompression
);
criterion_main!(benches);
