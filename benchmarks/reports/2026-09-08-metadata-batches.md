# Ordered metadata lookup batches — 2026-09-08

**The optimization was reverted.** Two paired runs did not establish a convincing fsck improvement. The final code change retains the regression tests; production lookup behavior matches main. Both benchmark runs and the measured candidate patch remain in the corpus.

The initial three-pair matrix changed full streamed `fsck --audit-only` at 65,601 records from **5.174 s to 5.263 s** (**+1.71%**). All **36 measured scans** passed their correctness gates.

| Records | Memory limit | Mode | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---|---:|---:|---:|---:|---:|
| 8,257 | 8,256 | Streamed records + spill | 0.816 | 0.752 | -7.84% | 42.7 | 42.5 |
| 8,257 | 8,257 | Cached records + spill | 0.536 | 0.464 | -13.44% | 47.0 | 46.7 |
| 8,257 | 8,258 | Cached records, no spill | 0.189 | 0.194 | +2.61% | 41.0 | 40.9 |
| 65,601 | 65,600 | Streamed records + spill | 5.174 | 5.263 | +1.71% | 143.5 | 141.3 |
| 65,601 | 65,601 | Cached records + spill | 2.842 | 2.905 | +2.22% | 179.4 | 179.4 |
| 65,601 | 65,602 | Cached records, no spill | 1.044 | 1.028 | -1.48% | 142.4 | 142.4 |

## Focused follow-up and decision

The initial 65,601-record streamed result was 1.71% slower. A second run therefore repeated that fixture with five pairs per mode, using the same immutable binaries and audits. All **30 additional scans** passed. Neither run is discarded.

| Memory limit | Mode | Baseline s | Candidate s | Change |
|---:|---|---:|---:|---:|
| 65,600 | Streamed records + spill | 4.822 | 4.724 | -2.03% |
| 65,601 | Cached records + spill | 2.511 | 2.376 | -5.39% |
| 65,602 | Cached records, no spill | 0.987 | 0.982 | -0.53% |

The target improved by only 2.03% in the follow-up, while the cached-but-spilled control moved by 5.39%. The target direction also reversed between runs. This evidence does not establish a repeatable benefit from sorting the metadata lookups. The candidate still executes one SQL query per distinct key; changing their order and reducing statement-cache checkouts did not produce a clear full-command gain on this fixture. Production `object_batch` was restored. The new functional tests are retained and checked against that restored implementation.

## Tested implementation

`TursoSnapshot::object_batch` previously called the single-record helper for each input key. Each lookup checked out a cached statement, queried, and decoded its record. The tested candidate sorts input indices by the composite `(namespace, native_id)` key inside the existing blocking job. It prepares one cached statement for the batch, queries and decodes each distinct key once, and restores results to their original input positions. Repeated keys receive cloned results, including repeated misses. Empty batches return immediately.

The query predicate and record decoder are unchanged. Invalid compressed data and a decoded record belonging to a different key still produce corruption errors. The method uses the same immutable snapshot connection. Sorting does not change successful result order, cardinality, or missing-key positions. Single-object lookups and payload-summary batch methods are unchanged.

The extra index vector is proportional to the caller's existing batch. Fsck's graph frontier remains bounded at 256 entries; no whole-repository cache is added. Sorting was intended to improve index-page locality, while deduplication avoids repeated reads and decompression. The standard fsck fixture has unique file contents, so its timings primarily exercise distinct-key lookups rather than duplicate-heavy batches.

## Baseline and measurement

Baseline is `a0a8b8941feca5553478c32b2e03c5bd2bf68d7d`. Its retained executable is the final candidate from the [streaming inventory investigation](2026-09-08-fsck-inventory.md); the committed production diff was verified against that receipt's exact source patch. Candidate adds only the metadata batch patch embedded in this receipt.

Both binaries use Rust 1.96.0, identical Cargo.lock, release optimization level 3, `--features cli`, and package-only `profile.release.package.casita.codegen-units=256`. Binary identities, compiler artifacts, source hashes, and build logs are retained. Both variants receive fresh timings; the previous report's medians are not used as this experiment's baseline.

The existing permanent `fsck` suite is registered in the manifest, `benchmark all`, revision comparisons, and dashboard normalization. Its standard fixtures contain 8,192 and 65,536 unique small files plus 65 directory records, real packed payloads, and one retained root. Previously retained fixtures are reused with all correctness audits repeated.

Each fixture of N records runs at memory limits N−1, N, and N+1: streamed metadata with spilled traversal sets, cached metadata with spilled sets, and fully in-memory traversal. The candidate uses the changed metadata batch method during graph discovery when its record cache is exceeded. The cached modes are controls. The 250,000-record frontier case was not run.

Every timed scan is a fresh CLI process after setup audits warm the repository. Memory-limit order is shuffled deterministically for each repetition, and binary order alternates. The initial table uses medians of three samples per cell; the focused table uses five. Wall time includes startup, open, and teardown; peak RSS covers the measured process, excluding fixture setup and checkout. Owned builds and tests finish before timing. The host is otherwise shared, and three pairs do not provide formal confidence intervals or justify a precise claim from small control differences.

## Correctness and validation

The permanent runner accepts only healthy output, exactly one root and the expected record and payload counts, unchanged revision, expected spill behavior, and no leftover spill files. Each fixture must pass before/after root-identity and checkout audits covering paths, node types, hashes, and executable bits. Binary hashes must remain unchanged. Incomplete matrices are rejected by dashboard normalization.

New tests cover a 2,051-key batch with duplicates and misses across multiple namespaces, including empty, prefix, NUL, and non-UTF-8 native IDs. They reverse the request order and repeat it, verify old/new snapshot isolation across a commit, and check empty batches. A second test injects invalid compressed data and a mismatched stored record, verifies corruption is reported, and checks a valid read still succeeds afterward.

For the measured candidate, **2 metadata batch regression tests**, **487 enabled library tests** (17 ignored), **2 online collection integration tests**, all-feature/all-target Clippy with warnings denied, formatting, diff whitespace checks, and `benchmark all --profile smoke --suites fsck --repetitions 1` passed. Dashboard normalization preserved all 12 initial observations with three successful samples each across six comparable workloads, plus six follow-up observations with five successful samples each.

After restoring production code, both new regression tests passed again, as did all-feature/all-target Clippy, formatting, and whitespace checks. All non-test code in `src/metadata/sqlite.rs` was verified to match main exactly.

The preceding read-only inspection profiled the baseline at 65,601 records with memory limit 65,600. Its healthy counts and exact fixture revision were verified. It showed comparison, allocation, and B-tree work with little SQL compilation. The receipt retains its compact symbols and raw-data identity. Flat CPU samples do not attribute time to this method; the paired benchmark measures the complete command.

These results cover healthy small-object fsck. Duplicate-heavy performance, repair, garbage collection, concurrent mutation throughput, and large-payload workloads were not benchmarked. No wall-clock regression threshold is introduced.

## Reproduction

Build baseline and candidate in separate checkouts with the same lockfile, retaining the CLI artifact identified by Cargo's JSON output. To reproduce the rejected candidate, apply `candidate_patch` from the receipt to the baseline revision; the final workspace retains only the added tests:

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
cargo test --locked --features cli --lib --no-run --message-format=json
```

Retain the library-test executable as the native fixture seeder. To construct fresh fixtures and run the matrix:

```console
python3 -m benchmarks.cli run fsck --profile standard --repetitions 3 --casita /path/to/candidate-casita --baseline-casita /path/to/baseline-casita --seed-probe /path/to/casita-lib-test --work-dir /new/fixture-directory --output benchmarks/results/metadata-batches-paired.json
```

This run instead passes `--reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures`, omitting `--seed-probe` and `--work-dir`. The [machine-readable receipt](2026-09-08-metadata-batches.json) contains all samples, summary medians, normalized observations, source/build identities, dependency lock, fixture provenance, validation logs, and the baseline profile. Large executable artifacts remain in the ignored results directory with recorded hashes.
