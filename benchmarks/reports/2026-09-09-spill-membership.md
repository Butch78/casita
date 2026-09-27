# Indexed spill membership queries — 2026-09-09

**Rejected.** Replacing cached point lookups with indexed multi-key SQL did not convincingly improve sustained-spill fsck. At 65,601 records, median wall time changed by **−0.9% at a 1,024-object limit** and **+0.1% at 4,096**. Individual pairs varied in both directions. The production change and its candidate-only tests were removed; the existing batched blocking jobs and cached point queries remain.

## Full-command results

Three matched pairs per memory limit, alternating immutable baseline and candidate CLIs. All 30 scans passed exact inventory, revision, expected-spill, and cleanup gates. Before/after checkout audits preserved the retained tree and root. Owned builds and tests had finished before the paired matrix.

| Memory limit | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---:|---:|---:|---:|
| 1,024 | 8.739 | 8.658 | -0.92% | 139.2 | 135.6 |
| 4,096 | 7.340 | 7.348 | +0.11% | 137.7 | 141.3 |
| 65,600 | 4.245 | 4.334 | +2.10% | 148.3 | 136.0 |
| 65,601 | 2.232 | 2.076 | -7.01% | 180.5 | 181.4 |
| 65,602 | 0.919 | 0.925 | +0.71% | 143.4 | 143.8 |

Values above are per-variant medians. The sustained-spill pairs were:

| Memory limit | Pair | Baseline s | Candidate s | Change |
|---:|---:|---:|---:|---:|
| 1,024 | 1 | 9.248 | 8.309 | -10.15% |
| 1,024 | 2 | 8.739 | 8.658 | -0.92% |
| 1,024 | 3 | 8.636 | 8.995 | +4.15% |
| 4,096 | 1 | 7.599 | 7.423 | -2.32% |
| 4,096 | 2 | 7.340 | 7.348 | +0.11% |
| 4,096 | 3 | 7.317 | 7.049 | -3.66% |

Median paired percentage changes were −0.9% at 1,024 and −2.3% at 4,096; these differ from ratios of marginal medians. The cache-boundary controls changed by +2.1%, −7.0%, and +0.7%. These mixed results on a shared host do not support keeping extra query machinery for the intended sustained-spill workload. No speedup is claimed.

## Candidate

Each existing blocking job retained the configured memory bound and 1,024-key cap. Within the job, the candidate moved up to 256 encoded keys into a `VALUES` CTE and joined the spill table on its primary-key index. Per-request ordinals preserved caller positions and duplicate hits. Power-of-two widths limited the prepared cache to nine query shapes; NULL padding could not match a key. Encoded keys moved into SQL parameters without cloning their bytes. Buffered keys were still answered in memory, and transaction cleanup remained in the existing connection wrapper.

New tests verified indexed lookup plans for all nine widths, parameter rebinding across hits/misses/padding, mixed buffered and persisted keys, duplicates, and SQL/job batch boundaries. All **506 enabled library tests** passed (20 ignored), including the 19 spill tests and existing quota, cancellation, abandoned-transaction, shared-graph, and exact-fsck-report checks. Eight integration tests, all-feature/all-target Clippy, 170 benchmark-harness tests, formatting, and diff checks also passed. Rejection is based on performance evidence.

## Corpus and reproduction

The existing `fsck` suite is registered in `benchmarks/manifest.json`, included in `benchmark all`, and supported by revision comparisons and the dashboard. Its standard cases retain sustained limits 1,024/4,096 and N−1/N/N+1 controls. No temporary-only benchmark was introduced. `benchmark all --profile smoke --suites fsck` passed its five limits (32, 128, 320, 321, 322); smoke timings are correctness evidence only and overlapped the full library test run.

Baseline is `d82b09aedc35c707db3b458ed13087c8dd5b46ba`. Its immutable binary was reused from the previous retained investigation after verifying both production-source hashes and the executable hash. Both builds use Rust 1.96.0, identical Cargo.lock, release optimization, CLI features, and package-only codegen-units=256. The JSON receipt retains baseline provenance, the exact rejected candidate patch, source/binary identities, Cargo artifact metadata, all raw samples and individual pairs, validation logs, and the smoke completion ledger.

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
benchmark run fsck --profile standard --files 65536 --repetitions 3 --casita benchmarks/results/2026-09-09-spill-membership/candidate-casita --baseline-casita benchmarks/results/2026-09-09-spill-membership/baseline-casita --reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures --output benchmarks/results/2026-09-09-spill-membership/paired.json
benchmark all --profile smoke --suites fsck --repetitions 1 --bin-dir benchmarks/results/2026-09-09-spill-membership/smoke-bin --output benchmarks/results/2026-09-09-spill-membership/all-smoke
```

To recreate the comparison, build the baseline commit in an isolated checkout, then apply `candidate_patch` from the JSON receipt and build the candidate with the same configuration. Copy the executable paths emitted by Cargo before changing revisions. For fresh fixtures, use the registered `--seed-probe` and `--work-dir` options instead of `--reuse-work-dir`; fixture construction and checkout audits remain outside timed fsck.

This run covers one healthy 65,601-record small-file fixture. It does not establish performance for larger frontier repositories, corrupt repositories, repair, or other graph shapes. Memory limits apply per traversal structure, while RSS includes the rest of fsck and its payload cache.
