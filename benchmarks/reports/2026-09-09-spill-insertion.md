# Batched fsck spill insertion — 2026-09-09

**Retained.** On the 65,601-record fixture, median full-fsck wall time fell by **46.3% at a 1,024-object memory limit** and **30.6% at 4,096**. Every sustained-spill pair improved. The three cache-boundary controls changed by −0.2%, −1.4%, and +2.3%.

## Full-command results

Three matched pairs per memory limit, using immutable baseline and candidate CLIs. The 30 measured scans all passed exact inventory, revision, spill, and cleanup gates. Untimed audits confirmed unchanged retained root and checkout contents. Owned builds and tests completed before paired measurements.

| Memory limit | Mode | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---|---:|---:|---:|---:|---:|
| 1,024 | Sustained spill | 18.480 | 9.920 | -46.32% | 164.2 | 139.0 |
| 4,096 | Sustained spill | 11.762 | 8.165 | -30.58% | 140.1 | 138.2 |
| 65,600 | Streamed metadata + spill | 5.002 | 4.992 | -0.19% | 141.8 | 147.4 |
| 65,601 | Cached metadata + spill | 2.451 | 2.418 | -1.35% | 182.0 | 180.4 |
| 65,602 | Cached metadata, no spill | 0.938 | 0.959 | +2.28% | 142.4 | 142.9 |

Wall values above are per-variant medians, not medians of paired percentage changes. Individual sustained-spill pairs show the shared-host variability:

| Memory limit | Pair | Baseline s | Candidate s | Change |
|---:|---:|---:|---:|---:|
| 1,024 | 1 | 18.480 | 9.920 | -46.32% |
| 1,024 | 2 | 19.020 | 17.241 | -9.36% |
| 1,024 | 3 | 13.259 | 9.906 | -25.29% |
| 4,096 | 1 | 11.707 | 8.945 | -23.59% |
| 4,096 | 2 | 11.879 | 8.165 | -31.26% |
| 4,096 | 3 | 11.762 | 7.892 | -32.91% |

At 1,024 objects, the median paired percentage reduction is 25.3%, compared with the 46.3% reduction between marginal medians. At 4,096, the median paired reduction is 31.3%, compared with 30.6% between marginal medians. The direction is consistent across all six sustained-spill pairs; the exact gain is workload- and host-dependent. Fully in-memory fsck was 2.3% slower in this run, within the small observed control variation; this is not an in-memory speedup claim.

An earlier one-repetition baseline preflight measured 14.227 s and 23.811 s at limits 1,024 and 4,096. Those timings are retained separately and are not substituted into the paired comparison.

## Change

`SpillSet::insert` probes membership before inserting each new key. Once a set has spilled, each disk probe uses a separate blocking job and connection lock. The new `insert_batch` amortizes those probes with the existing bounded `contains_batch`, then inserts keys into the write buffer in caller order. SQL membership statements remain point queries inside each blocking job; this change does not introduce multi-row INSERT SQL.

Each group is capped by both 1,024 and the remaining write-buffer capacity. The group cannot flush midway, so the in-memory set detects repeated new keys before they can disappear into a flush. Existing keys and later duplicates report false; only the first insertion reports true. Flushes keep the existing sorted transaction and spill-budget checks. A full buffer left by cancellation is flushed before accepting more keys. Errors retain the existing possibility of a partially updated temporary set.

Fsck uses this for reachable objects, physical blob/chunk inventories, logical payloads, and referenced chunks. Inventory buffers are capped by the configured memory limit and 1,024. The graph frontier also respects remaining traversal capacity, admitting at most the one extra key needed to detect an exceeded limit. Final inventory comparison, issue ordering, payload verification, and immutable snapshot checks retain their existing behavior.

## Permanent corpus

The existing `fsck` entry remains registered in the manifest, all-suite runner, revision comparisons, and dashboard. Standard/frontier defaults now include limits 1,024 and 4,096 alongside N−1/N/N+1. Smoke includes 32 and 128 alongside those boundaries. `--memory-objects` replaces the defaults for explicit sweeps. This measures sustained spilling, which the original N±1 matrix largely missed.

This investigation ran the 65,536-file fixture (65,601 records) with all five limits. `benchmark all --profile smoke --suites fsck` passed all five smoke cases. Dashboard normalization preserves ten paired observations and five smoke observations. Full standard defaults also include the existing 8,192-file fixture; that additional size and the frontier were not timed here.

## Validation and reproduction

**504 enabled library tests** passed (20 ignored), including 17 spill-set tests, a shared 260-object graph with repeated references across the frontier, exact garbage/corruption report comparisons, traversal-limit enforcement, quota failure, recovery from a cancelled flush, and cancellation after actual spill-file creation during both closure verification and fsck. Eight application/online-collection integration tests, all-feature/all-target Clippy, 170 benchmark-harness tests, formatting, and diff checks passed.

Baseline is `4c1702559eb2581475fde0f066520eac135556ba`. Both CLIs use Rust 1.96.0, identical Cargo.lock, release optimization, CLI features, and package-only codegen-units=256. The fixture is reused from the permanent full-fsck suite; before/after audits repeat. The JSON receipt embeds the production/corpus patches, source and binary hashes, Cargo artifact records, fixture origin, all timings and correctness output, normalized results, and validation logs.

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
benchmark run fsck --files 65536 --memory-objects 1024,4096 --repetitions 1 --casita benchmarks/results/2026-09-09-spill-insertion/baseline-casita --reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures --output benchmarks/results/2026-09-09-spill-insertion/baseline-sustained.json
benchmark run fsck --profile standard --files 65536 --repetitions 3 --casita benchmarks/results/2026-09-09-spill-insertion/candidate-casita --baseline-casita benchmarks/results/2026-09-09-spill-insertion/baseline-casita --reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures --output benchmarks/results/2026-09-09-spill-insertion/paired.json
benchmark all --profile smoke --suites fsck --repetitions 1 --bin-dir benchmarks/results/2026-09-09-spill-insertion/smoke-bin --output benchmarks/results/2026-09-09-spill-insertion/all-smoke
```

Build the baseline revision and candidate with the recorded configuration, and copy the executable paths emitted by Cargo. The commands above use retained local copies. For new fixtures, use the suite’s `--seed-probe` and `--work-dir` options instead of `--reuse-work-dir`; fixture construction and checkout audits are outside timed fsck.
