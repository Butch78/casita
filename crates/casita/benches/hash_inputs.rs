//! Cold payload writes and hash sizes from immutable Git blobs or boundary fixtures.
mod bench_util;

use casita::experimental::{BlobId, BlobStore, ChunkId, ChunkMeta, DEFAULT_AVG_CHUNK_SIZE};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMITS: [usize; 11] = [
    0,
    64,
    1024,
    16384,
    65536,
    131072,
    262144,
    524288,
    1048576,
    4194304,
    usize::MAX,
];

fn histogram(sizes: impl Iterator<Item = usize>) -> Value {
    let mut counts = [0_u64; LIMITS.len()];
    let mut bytes = [0_u64; LIMITS.len()];
    for size in sizes {
        let index = LIMITS.iter().position(|&limit| size <= limit).unwrap();
        counts[index] += 1;
        bytes[index] += size as u64;
    }
    let count: u64 = counts.iter().sum();
    let total_bytes: u64 = bytes.iter().sum();
    let mut cumulative_count = 0;
    let mut cumulative_bytes = 0;
    let buckets: Vec<_> = LIMITS.iter().enumerate().map(|(index, &limit)| {
        cumulative_count += counts[index];
        cumulative_bytes += bytes[index];
        json!({
            "upper_bound_bytes": (limit != usize::MAX).then_some(limit),
            "count": counts[index], "bytes": bytes[index],
            "cumulative_count": cumulative_count, "cumulative_bytes": cumulative_bytes,
            "cumulative_count_percent": (count != 0).then(|| cumulative_count as f64 * 100.0 / count as f64),
            "cumulative_byte_percent": (total_bytes != 0).then(|| cumulative_bytes as f64 * 100.0 / total_bytes as f64),
        })
    }).collect();
    json!({"count": count, "bytes": total_bytes, "buckets": buckets})
}

fn git(repository: &std::ffi::OsStr, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn corpus() -> (Value, Vec<Vec<u8>>) {
    let Some(repository) = std::env::var_os("CASITA_HASH_REPOSITORY") else {
        let min = DEFAULT_AVG_CHUNK_SIZE as usize / 2;
        let max = DEFAULT_AVG_CHUNK_SIZE as usize * 2;
        let sizes = [
            0,
            64,
            1024,
            16384,
            65536,
            min - 1,
            min,
            min + 1,
            DEFAULT_AVG_CHUNK_SIZE as usize,
            max - 1,
            max,
            max + 1,
            1048576,
            4194304,
        ];
        return (
            json!({"kind": "synthetic-boundaries", "seed": 83}),
            sizes
                .into_iter()
                .flat_map(|size| [bench_util::random_bytes(83, size), vec![0; size]])
                .collect(),
        );
    };
    let revision = String::from_utf8(git(&repository, &["rev-parse", "HEAD^{commit}"])).unwrap();
    let revision = revision.trim();
    let listing = git(&repository, &["ls-tree", "-r", "-z", "-l", revision]);
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut files = Vec::new();
    let mut total = 0_usize;
    let mut skipped = 0;
    for entry in listing
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let header = entry.split(|&byte| byte == b'\t').next().unwrap();
        let header = std::str::from_utf8(header).unwrap();
        let fields: Vec<_> = header.split_whitespace().collect();
        if !matches!(fields[0], "100644" | "100755") {
            skipped += 1;
            continue;
        }
        let size: usize = fields[3].parse().unwrap();
        total = total.checked_add(size).unwrap();
        assert!(
            total <= 1024 * 1024 * 1024,
            "corpus exceeds the explicit 1 GiB in-memory limit"
        );
        writeln!(input, "{}", fields[2]).unwrap();
        input.flush().unwrap();
        let mut response = String::new();
        output.read_line(&mut response).unwrap();
        assert_eq!(response.trim(), format!("{} blob {size}", fields[2]));
        let mut bytes = vec![0; size];
        output.read_exact(&mut bytes).unwrap();
        let mut newline = [0];
        output.read_exact(&mut newline).unwrap();
        assert_eq!(newline, [b'\n']);
        files.push(bytes);
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    (
        json!({"kind": "git-committed-regular-files", "revision": revision,
        "tree_listing_blake3": blake3::hash(&listing).to_hex().to_string(),
        "skipped_symlinks_and_submodules": skipped,
        "selection": "every regular file occurrence, including duplicate contents; no working-tree reads"}),
        files,
    )
}

fn hash_inputs(c: &mut Criterion) {
    let (source, files) = corpus();
    let identity = source["revision"]
        .as_str()
        .unwrap_or("synthetic-boundaries")
        .to_owned();
    let avg = DEFAULT_AVG_CHUNK_SIZE;
    let min = avg / 2;
    let max = avg * 2;
    let expected: Vec<_> = files
        .iter()
        .map(|bytes| {
            let chunks: Vec<_> =
                fastcdc::v2020::FastCDC::new(bytes, min as usize, avg as usize, max as usize)
                    .map(|chunk| ChunkMeta {
                        digest: ChunkId::new(
                            blake3::hash(&bytes[chunk.offset..chunk.offset + chunk.length]).into(),
                        ),
                        size: chunk.length as u64,
                    })
                    .collect();
            assert_eq!(
                chunks.iter().map(|chunk| chunk.size).sum::<u64>(),
                bytes.len() as u64
            );
            (BlobId::new(blake3::hash(bytes).into()), chunks)
        })
        .collect();
    let mut existing_reuse = 0;
    let mut added_reuse = 0;
    let mut independently_hashed = Vec::new();
    for (bytes, (_, chunks)) in files.iter().zip(&expected) {
        if chunks.len() == 1 && bytes.len() < min as usize {
            existing_reuse += 1;
        } else if chunks.len() == 1 && bytes.len() < max as usize {
            added_reuse += 1;
        } else {
            independently_hashed.extend(chunks.iter().map(|chunk| chunk.size as usize));
        }
    }
    let report = json!({
        "schema": "casita.hash-inputs.v1", "source": source,
        "scope": "cold payload writer inputs; excludes metadata, repository verification, readback verification, and incremental-import skip decisions",
        "chunking": {"minimum": min, "average": avg, "maximum": max},
        "whole_file_inputs": histogram(files.iter().map(Vec::len)),
        "reference_storage_chunks": histogram(expected.iter().flat_map(|(_, chunks)| chunks.iter().map(|chunk| chunk.size as usize))),
        "predicted_independent_chunk_hash_inputs": histogram(independently_hashed.into_iter()),
        "existing_small_file_digest_reuse_count": existing_reuse,
        "additional_eof_digest_reuse_count": added_reuse,
        "reuse_method": "derived from reference chunks and current writer EOF rules; not runtime call instrumentation",
        "correctness": "every cold write checks whole digest, exact reference chunk IDs/sizes, and complete readback",
    });
    let rt = bench_util::runtime();
    let verified = std::cell::Cell::new(false);
    let mut group = c.benchmark_group("hash_inputs");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(
        files.iter().map(|bytes| bytes.len() as u64).sum(),
    ));
    group.bench_function(BenchmarkId::new("whole_file_blake3", &identity), |b| {
        b.iter(|| {
            for (bytes, (digest, _)) in files.iter().zip(&expected) {
                assert_eq!(
                    BlobId::new(blake3::hash(std::hint::black_box(bytes)).into()),
                    *digest
                );
            }
        })
    });
    group.bench_function(BenchmarkId::new("cold_payload_writes", &identity), |b| {
        b.iter_custom(|iterations| {
            rt.block_on(async {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let (store, _) = bench_util::memory_store(avg);
                    for (bytes, (digest, chunks)) in files.iter().zip(&expected) {
                        let start = Instant::now();
                        let actual = bench_util::write_blob(&store, bytes).await;
                        elapsed += start.elapsed();
                        assert_eq!(actual, *digest);
                        assert_eq!(store.chunks(digest).await.unwrap().unwrap(), *chunks);
                        assert_eq!(store.read_to_vec(digest).await.unwrap().unwrap(), *bytes);
                    }
                }
                verified.set(true);
                elapsed
            })
        })
    });
    group.finish();
    if let Some(path) = std::env::var_os("CASITA_HASH_REPORT") {
        // Publish only after all executed correctness gates have succeeded.
        assert!(
            verified.get(),
            "report requires the cold_payload_writes case to run"
        );
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

criterion_group!(benches, hash_inputs);
criterion_main!(benches);
