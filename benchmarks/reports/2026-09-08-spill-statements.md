# Reusing traversal spill statements — 2026-09-08

Reusing prepared statements in `SpillSet` reduces full streamed `fsck --audit-only` at 65,601 records from **11.619 s to 8.100 s**, a **30.29% reduction** (1.43× faster). With the metadata cache retained but traversal sets spilled, it improves from **8.380 s to 5.888 s**, a **29.74% reduction**. A separate paired CPU profile reduces identifiable SQL parsing, translation, and bytecode construction from **22.29% to 0.27% of sampled user-mode cycles**.

## Implementation and transaction behavior

Membership checks and ordered pages use Turso's connection statement cache. Each flush obtains one cached insert statement inside its existing transaction and reuses it for every pending key. The statement is dropped before commit. SQL predicates, ordering, page limits, flush thresholds, transaction boundaries, spill budgets, and queue SQL are unchanged.

Turso's cached preparation bypasses the dangling-transaction cleanup performed by its ordinary query and execute methods. The serialized connection helper now executes an empty batch before each operation, invoking that cleanup without compiling a query. This preserves rollback before reads after an abandoned write, including errors or panics. A regression test primes the cache, triggers a constraint failure after a successful uncommitted insert, and verifies that the next cached read cannot see the abandoned write. Another test alternates hits, misses, duplicate inserts, flushes, and repeated ordered scans with four page sizes, then checks spill-file cleanup.

## Matched results

Baseline is `2d1b6a520f61e501a6e898143562bb9d6b8e54c9`; candidate is that revision plus the `src/spill.rs` patch embedded in the receipt. Both CLIs were freshly built with Rust 1.96.0, identical locked dependencies, release optimization level 3, `--features cli`, and package-only `profile.release.package.casita.codegen-units=256`. This override matches the preceding investigation and reduces compiler memory pressure. Both builds succeeded. Immutable binary hashes, build logs, the exact source patch, and the dependency lock are retained in the receipt.

The permanent `fsck` suite ran three repetitions of each binary at each size and limit: **36 successful measured processes**. The three limits are shuffled deterministically per repetition, and binary order alternates between repetitions. Setup audits warm the repository; each timed command is a fresh CLI process. Wall time includes open and teardown. RSS is the median of per-process peaks, excluding fixture setup and checkout.

| Records | Memory limit | Mode | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---|---:|---:|---:|---:|---:|
| 8,257 | 8,256 | Streamed records + spill | 1.362 | 0.953 | −30.04% | 55.1 | 56.1 |
| 8,257 | 8,257 | Cached records + spill | 1.177 | 0.724 | −38.54% | 60.6 | 58.8 |
| 8,257 | 8,258 | Cached records, no spill | 0.164 | 0.163 | −0.58% | 41.7 | 41.7 |
| 65,601 | 65,600 | Streamed records + spill | 11.619 | 8.100 | −30.29% | 152.0 | 152.0 |
| 65,601 | 65,601 | Cached records + spill | 8.380 | 5.888 | −29.74% | 192.5 | 194.9 |
| 65,601 | 65,602 | Cached records, no spill | 1.015 | 1.043 | +2.75% | 143.5 | 144.3 |

Spill file counts and peak spill bytes are identical between variants in every cell. Limits N−1, N, and N+1 distinguish streamed metadata with spilled traversal sets, cached metadata with spilled sets, and fully in-memory traversal. All spilled cases opened four spill files; in-memory cases opened none.

The no-spill controls vary by approximately −1 ms and +28 ms. These are shared-host measurements with three pairs, without formal confidence intervals or a clock-based regression gate. Owned builds and checks finished before the paired measurements, and profiling followed afterward. The host was not otherwise isolated. Compare against this fresh baseline: the preceding report used an older revision and different run conditions, so its 9.529-second result is not this experiment's baseline.

## CPU profiles

Separate sequential `perf record -F 999 -e cycles:u` runs used the same 65,601-record repository with limit 65,601. Both checked exactly one root and all records and payloads at the original revision. Baseline collected 8,630 samples; candidate collected 4,939; both reported zero lost samples. Their timings are excluded from the results table.

| Symbol family | Baseline self-cycle share | Candidate self-cycle share |
|---|---:|---:|
| `turso_parser::` | 5.91% | 0.17% |
| `turso_core::translate::` | 13.17% | 0.10% |
| `turso_core::vdbe::builder::` | 3.21% | 0.00% |
| Sum | 22.29% | 0.27% |

These sums use displayed symbol self percentages rounded to two decimals. They measure sampled user CPU cycles, not wall-time attribution; zero means no share visible at that precision. The change removes the targeted repeated compilation cost, while storage operations, allocations, dispatch, and I/O remain.

## Corpus and correctness

This investigation uses the existing permanent `fsck` suite registered in `benchmarks/manifest.json`, `benchmark all`, revision comparisons, and dashboard normalization. No temporary benchmark replaces the corpus. The standard fixtures contain 8,192 and 65,536 unique small files plus 65 directory records, one retained root, and real packed payloads. The retained fixtures from the [previous investigation](2026-09-08-fsck.md) were reused, with all audits repeated. Their source manifest hashes and original native seeder identities are in the receipt.

Every measured process passed exact root/object/payload counts, unchanged revision, healthy output, expected spill behavior, and spill-file cleanup. Both fixtures passed before/after retained-root and checkout checks, including paths, node types, payload hashes, and executable bits. Both executable hashes were unchanged after the matrix. Dashboard normalization produced 12 observations, each with three successful samples, across six comparable workloads.

Validation passed: **10 spill tests**, **480 enabled library tests** (17 ignored), all-feature/all-target Clippy with warnings denied, Rust formatting, diff whitespace checks, and `benchmark all --profile smoke --suites fsck --repetitions 1`. The full library run includes spill cancellation and crash recovery coverage. Tests and build logs are embedded in the receipt. The benchmark runner was unchanged.

The permanent frontier case at 250,000 records was not run. This measures healthy auditing of small packed objects across configured thresholds, not garbage collection, repair, concurrent writes, or large-payload throughput.

## Reproduction

Build and retain separate baseline and patched candidate executables with the same compiler and lockfile:

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
cargo test --locked --features cli --lib --no-run --message-format=json
```

Retain the library-test executable identified by Cargo's JSON output as the fixture seeder. To create new fixtures and run the same matrix:

```console
python3 -m benchmarks.cli run fsck --profile standard --repetitions 3 --casita /path/to/candidate-casita --baseline-casita /path/to/baseline-casita --seed-probe /path/to/casita-lib-test --work-dir /new/fixture-directory --output benchmarks/results/spill-statements-paired.json
```

This run instead passed `--reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures`, omitting `--seed-probe` and `--work-dir`. This repeats the audits and excludes setup from timing. `benchmark all` supplies the seeder automatically. For profiling, run each binary sequentially:

```console
perf record -F 999 -e cycles:u -o /path/to/variant.perf.data -- /path/to/variant-casita --repository /fixture-directory/65536/repository --spill-memory-objects 65601 --spill-bytes 4294967296 fsck --audit-only
perf report -i /path/to/variant.perf.data --stdio --no-children --sort symbol --percent-limit 0 --field-separator ';'
```

The [machine-readable receipt](2026-09-08-spill-statements.json) contains all samples, medians, normalized observations, source/build identities, validation logs, fixture provenance, and compact CPU profiles. Large binary artifacts and raw perf data remain in the ignored results directory with their hashes recorded.
