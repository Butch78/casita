# Right-size blob verification scratch space

Blob verification allocated and initialized its configured 64 KiB scratch
buffer for every payload. Tar finalization supplies the stored payload's
length through `BlobPayloadReader`, so a 1 KiB file can use 1 KiB of scratch:
63 KiB less per verification, or 98.4% of this buffer. This is an allocation
size reduction, not an end-to-end throughput or total-memory claim.

The production change in `src/format.rs` caps scratch space at the available
length hint. Unknown lengths retain the configured capacity. The buffer is
always at least one byte; EOF, complete hashing, and payload limits remain
mandatory even when a hint is zero or understates the actual payload.

The [raw results](2026-09-11-verification-buffers.json) retain the production
patch, common benchmark source, build information, executable fingerprints,
per-case scratch sizes, and correctness logs.

| Known payload size | Baseline scratch | Candidate scratch |
|---:|---:|---:|
| 0 | 65,536 | 1 |
| 1 | 65,536 | 1 |
| 1,024 | 65,536 | 1,024 |
| 16,384 | 65,536 | 16,384 |
| 65,535 | 65,536 | 65,535 |
| 65,536 | 65,536 | 65,536 |
| 65,537 | 65,536 | 65,536 |
| 262,144 | 65,536 | 65,536 |

All eight unknown-length cases retain 65,536 bytes. The measurement records
the largest initialized scratch slice supplied to `PayloadReader::read`.
It does not measure allocator metadata, other buffers, RSS, or elapsed time.
For the existing 256 × 1 KiB tar fixture, these buffer lengths imply 15.75 MiB
less aggregate scratch initialization across file verifications, not 15.75 MiB
less peak resident memory.

The initial baseline build incorrectly reused the candidate executable through
the shared Cargo build cache. Identical hashes and the baseline's unexpected
buffer sizes exposed that mistake. That attempt was rejected, the baseline
sources were marked for rebuilding, and the comparison uses the rebuilt binary.
Use separate Cargo target and build directories for future comparisons.

## Permanent cases and validation

`verification_buffers` is part of the existing `optimization` executable,
registered in `benchmarks/manifest.json` and included in `benchmark all`.
Both builds pass all 16 cases, checking full content identity, complete size,
EOF, and the configured buffer bound. Sizes bracket the 64 KiB cap.

The candidate also passes five format tests (including incorrect length hints,
zero-capacity configuration, and payload-limit enforcement), 39 archive tests,
all 16 release tar benchmark cases, 12 affected Python harness tests,
all-features/all-targets Clippy, formatting, and whitespace checks.

```sh
# Correctness and scratch sizes, without timing:
devenv shell cargo bench --features experimental --bench optimization -- --test verification_buffers
# Timing on a quiet host, with the same correctness gates:
devenv shell cargo bench --features experimental --bench optimization -- verification_buffers
# Existing end-to-end archive correctness matrix:
devenv shell cargo bench --features experimental --bench tar_import -- --test
```

For the baseline, use commit `843497a9fa7833d8661fb46cda362dc23bb27840` and
copy `shared_benchmark_source` from the JSON to `benches/optimization.rs`.
Apply the retained `candidate_patch` only to the candidate checkout. Build
each with its own `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR`, then execute
the resulting benchmark with `--test verification_buffers`. No FastCDC version
or adapter changes are involved in this comparison.
