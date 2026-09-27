# Batching fsck spill membership checks — 2026-09-08

Batched spill-set membership checks reduce full streamed `fsck --audit-only` at 65,601 records from **7.904 s to 6.549 s**, a **17.14% reduction** (1.21× faster). All **36 measured scans** passed their correctness gates.

| Records | Memory limit | Mode | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---|---:|---:|---:|---:|---:|
| 8,257 | 8,256 | Streamed records + spill | 1.009 | 0.804 | -20.30% | 45.0 | 44.4 |
| 8,257 | 8,257 | Cached records + spill | 0.784 | 0.589 | -24.93% | 48.7 | 49.0 |
| 8,257 | 8,258 | Cached records, no spill | 0.182 | 0.191 | +4.69% | 41.5 | 41.5 |
| 65,601 | 65,600 | Streamed records + spill | 7.904 | 6.549 | -17.14% | 152.0 | 151.2 |
| 65,601 | 65,601 | Cached records + spill | 5.229 | 4.051 | -22.53% | 192.2 | 188.3 |
| 65,601 | 65,602 | Cached records, no spill | 1.013 | 1.004 | -0.84% | 143.9 | 144.4 |

The no-spill controls changed by approximately +9 ms at 8,257 records and −9 ms at 65,601 records. Spill file counts and peak spill bytes were identical between variants in every cell. At 65,601 records, median system CPU time dropped from 2.41 s to 1.80 s in streamed mode and from 1.55 s to 0.86 s in cached-but-spilled mode. Those CPU measurements support reduced scheduling overhead but are not a direct attribution of wall time.

## Implementation

Fsck checks physical manifest presence and root reachability before verifying each record. Previously, each spilled membership check acquired the connection and dispatched a separate blocking job. The new `SpillSet::contains_batch` reuses one prepared lookup statement across a bounded group of keys in one blocking job, returning flags in input order, including duplicates. Buffered keys are checked in memory first.

Fsck keeps its existing physical scan ordering and 65,536-record window. It builds temporary payload and key-reference buffers in groups of at most 1,024 records. Each spill job encodes at most `min(1024, max_memory_objects)` keys, with a minimum batch size of one. Only the two vectors of result flags span the scan window; the total repository inventory is never materialized by the new code. In-memory object keys are borrowed without cloning their namespace strings.

Record verification, issue emission, traversal limits, spill thresholds, and transaction cleanup retain their existing behavior. Both queried sets are stable during the verification phase. The final physical inventory scans and insertion-time deduplication still use individual membership checks. The optimization adds no concurrent database operations.

## Method and correctness

This uses the existing permanent `fsck` suite, registered in `benchmarks/manifest.json`, `benchmark all`, revision comparisons, and dashboard normalization. The standard matrix measures both sides of the configured metadata-cache and traversal-spill cutoffs at 8,257 and 65,601 records. The frontier case at 250,000 records is available in the corpus but was not run here.

Baseline is `ad3e944815988adf8a1ab80f999649e106679986`. Its retained binary comes from the [statement reuse investigation](2026-09-08-spill-statements.md): the production source changes from that investigation's base to current main were verified to exactly match the prior candidate patch. Candidate is this baseline plus the `src/spill.rs` and `src/repository.rs` patch embedded in the receipt. Both use Rust 1.96.0, the same Cargo.lock, release optimization level 3, `--features cli`, and the package-only override `profile.release.package.casita.codegen-units=256`.

The paired timings are fresh for both binaries; prior report timings are not used as the baseline. The retained packed fixtures from the first fsck investigation were reused, with every audit repeated. Each contains one retained root, unique small file payloads, and 65 directory records. Source manifest hashes and original fixture-builder provenance are retained in the receipt.

Each timed scan is a fresh CLI process after setup audits warm the repository. The three memory limits are shuffled deterministically per repetition; binary order alternates between repetitions. Wall time includes repository open and teardown. Peak RSS covers the measured CLI process, excluding fixture construction and checkout. The table uses medians of three samples per cell.

Every accepted scan must report exactly one root, the expected record and payload counts, the original revision, healthy output, expected spill behavior, and no leftover spill files. Both fixtures must pass before/after root-identity and checkout audits covering paths, node types, hashes, and executable bits. Binary hashes must remain unchanged. Incomplete matrices are rejected by dashboard normalization.

These are shared-host measurements with three pairs and no formal confidence intervals. Owned builds and checks finish before timing; the host is not otherwise isolated. Small control differences should not be treated as precise regressions or improvements. No wall-clock regression threshold is introduced. The results describe healthy auditing of small packed objects, not repair, garbage collection, concurrent writes, or large-payload throughput.

## Validation

The new batch test checks more than 6,000 queries across in-memory, spilled, and partially buffered sets, including repeated hits, misses, duplicate inputs, empty batches, multiple disk batches, and cleanup. The rollback regression now checks batched reads immediately after an abandoned write. The fsck regression compares complete reports, including issue order, between in-memory and forced-spill execution for both collectible staging and a missing live payload; only spill metrics are normalized.

**11 spill tests**, **481 enabled library tests** (17 ignored), **2 online collection integration tests**, all-feature/all-target Clippy with warnings denied, formatting, diff whitespace checks, and `benchmark all --profile smoke --suites fsck --repetitions 1` passed. Dashboard normalization preserved all 12 observations with three successful samples each across six comparable workloads.

An initial iterator-based API passed library tests but failed the all-target check because Rust could not prove the spawned fsck future's closure lifetimes. The final API takes slices and passes that integration compilation check. The initial build is excluded from all benchmark timings; the failed diagnostic is retained in the receipt.

## Reproduction

Build the baseline and candidate in separate checkouts with the same lockfile and retain each executable identified by Cargo's JSON output:

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
cargo test --locked --features cli --lib --no-run --message-format=json
```

Retain the library-test executable as the fixture seeder. To construct fresh fixtures and run the standard matrix:

```console
python3 -m benchmarks.cli run fsck --profile standard --repetitions 3 --casita /path/to/candidate-casita --baseline-casita /path/to/baseline-casita --seed-probe /path/to/casita-lib-test --work-dir /new/fixture-directory --output benchmarks/results/spill-membership-paired.json
```

This run instead uses `--reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures`, omitting `--seed-probe` and `--work-dir`. The receipt records exact measured commands, all raw samples, summary medians, normalized observations, build/source identities, dependency lock, and validation logs. Large executable artifacts remain in the ignored results directory with their hashes recorded.

See the [machine-readable receipt](2026-09-08-spill-membership.json).
