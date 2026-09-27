# Streaming fsck inventory differences — 2026-09-08

Streaming inventory differences reduce full streamed `fsck --audit-only` at 65,601 records from **5.804 s to 4.535 s**, a **21.87% reduction** (1.28× faster). All **36 measured scans** passed their correctness gates.

| Records | Memory limit | Mode | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---|---:|---:|---:|---:|---:|
| 8,257 | 8,256 | Streamed records + spill | 0.747 | 0.568 | -24.00% | 44.3 | 42.7 |
| 8,257 | 8,257 | Cached records + spill | 0.500 | 0.346 | -30.79% | 47.7 | 46.9 |
| 8,257 | 8,258 | Cached records, no spill | 0.149 | 0.153 | +2.69% | 40.8 | 41.1 |
| 65,601 | 65,600 | Streamed records + spill | 5.804 | 4.535 | -21.87% | 151.1 | 143.8 |
| 65,601 | 65,601 | Cached records + spill | 3.472 | 2.115 | -39.07% | 187.3 | 179.4 |
| 65,601 | 65,602 | Cached records, no spill | 0.885 | 0.862 | -2.69% | 143.2 | 142.3 |

At 65,601 records with metadata cached and traversal sets spilled, wall time falls from **3.472 s to 2.115 s**, a **39.07% reduction**. The in-memory controls differ by approximately +4 ms at 8,257 records and −24 ms at 65,601 records; these small shared-host differences do not establish a precise performance change.

At 65,601 records, median peak spill usage falls from approximately **56.1 MiB to 45.2 MiB** in both spilled modes, about 19% lower. Both variants open four spill files. Streamed-mode peak RSS falls from 151.1 MiB to 143.8 MiB; cached-but-spilled RSS falls from 187.3 MiB to 179.4 MiB. The raw measurements are retained in the receipt.

## Change

The final fsck inventory passes used to ask the logical payload or referenced chunk set about every physical key individually. Both sides already support canonical ordered streams. `SpillSet::into_difference` now merges those streams, keeping one key of lookahead and yielding only keys absent from the reference set. Fsck uses it for both payload and chunk inventories.

This removes per-key SQL membership lookups and their blocking jobs from these two passes. Spilled inputs are read through the existing bounded page iterator. In-memory inputs use their ordered set iterators. Payload counts are captured before consuming the reference set, and findings retain canonical payload order followed by canonical chunk order.

The operation consumes both sets and releases their temporary files when complete or dropped. Existing stream preparation flushes pending keys if a set has already spilled, so streaming the reference side can add a final buffer flush. It does not create storage for a set that fits in memory. Spill budget checks and cleanup remain active.

## Baseline and method

Baseline is the completed [batched membership implementation](2026-09-08-spill-membership.md), identified by its immutable executable and the exact source patch relative to `ad3e944815988adf8a1ab80f999649e106679986`. Those changes were still uncommitted when this experiment began. The retained binary and source patch were verified against the prior receipt before editing. Candidate adds the streaming inventory difference; the receipt contains both the incremental patch and the full patch relative to main.

Both executables use Rust 1.96.0, identical locked dependencies, release optimization level 3, `--features cli`, and the package-only override `profile.release.package.casita.codegen-units=256`. Build identities and logs are retained. Timings for both variants are fresh; the previous report's medians are not substituted for this run's baseline.

The permanent `fsck` suite is registered in the manifest, `benchmark all`, revision comparisons, and dashboard normalization. The standard fixtures contain 8,192 and 65,536 unique small files plus 65 directory records, real packed payloads, and one retained root. The retained fixtures from the first fsck investigation were reused with all audits repeated.

For each fixture of N records, memory limits N−1, N, and N+1 respectively exercise streamed metadata plus spilled traversal sets, cached metadata plus spilled sets, and fully in-memory traversal. The 250,000-record frontier case remains in the corpus but was not run.

Each measured scan runs in a new CLI process after setup audits warm the repository. The limits are shuffled deterministically per repetition and binary order alternates across repetitions. Results are medians of three samples per variant, size, and limit. Wall time includes open and teardown; process peak RSS excludes fixture setup and checkout. Owned builds, tests, and profiling finish before timing. The host is otherwise shared, with no formal confidence intervals or wall-clock regression gate.

## Correctness and validation

The benchmark accepts only healthy output with exactly one root and the expected record and payload counts, unchanged revision, expected spill behavior, and no leftover spill files. Each fixture must pass before/after retained-root identity and checkout audits covering paths, node types, content hashes, and executable bits. Executable hashes must stay unchanged. Incomplete matrices are rejected by normalization.

New tests compare streamed differences against standard ordered sets across multiple disk pages and all four combinations of in-memory and spilled inputs. They cover empty, equal, overlapping, and disjoint sets, either side exhausting first, partially buffered sets, and dropping a stream after its first result. A repository test checks exact unreferenced payload/chunk findings, counts, and issue order against three forced-spill limits.

**4 new difference tests**, **485 enabled library tests** (17 ignored), **2 online collection integration tests**, all-feature/all-target Clippy with warnings denied, formatting, diff whitespace checks, and `benchmark all --profile smoke --suites fsck --repetitions 1` passed. Dashboard normalization preserved all 12 observations with three successful samples each across six comparable workloads.

The baseline CPU profile was collected before builds and timing, at 65,601 records with metadata retained and sets spilled. Its exact fixture revision and healthy counts were verified. Symbol samples include comparison, allocation, and B-tree work; these flat samples do not isolate inventory-pass wall time. The source identifies the unnecessary point lookups; the paired full-command benchmark measures their replacement. The receipt retains the compact profile, raw-data hash, and profiler output. Profile timings are excluded from the results table.

This experiment measures healthy small-object fsck. Garbage findings are covered by correctness tests; repair, concurrent mutation performance, garbage collection, and large-payload throughput were not benchmarked.

## Reproduction

Build baseline and candidate in separate checkouts with the same lockfile, applying the corresponding patches from the receipt, and retain the executable paths identified by Cargo's JSON output:

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
cargo test --locked --features cli --lib --no-run --message-format=json
```

Retain the library-test executable as the native fixture seeder. To construct new fixtures and run the same matrix:

```console
python3 -m benchmarks.cli run fsck --profile standard --repetitions 3 --casita /path/to/candidate-casita --baseline-casita /path/to/baseline-casita --seed-probe /path/to/casita-lib-test --work-dir /new/fixture-directory --output benchmarks/results/fsck-inventory-paired.json
```

This run instead uses `--reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures`, omitting `--seed-probe` and `--work-dir`. All audits repeat. The [machine-readable receipt](2026-09-08-fsck-inventory.json) retains commands, samples, medians, normalized observations, source/build identities, dependency lock, fixture provenance, validation logs, and the baseline profile. Large executables and raw perf data remain in the ignored results directory with recorded hashes.
