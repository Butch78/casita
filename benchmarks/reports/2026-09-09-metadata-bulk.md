# Bounded metadata SQL batches — 2026-09-09

**Retained.** Direct 256-key batch reads are 17–38% faster across the measured sizes and request patterns. Full-fsck improvement remains inconclusive; the controls vary by similar or greater amounts.

## Direct reads

Median milliseconds for 4,096 requests, grouped into 256-key API calls. Each process has three warm iterations; the table takes the median of three process means. All six processes passed exact output and snapshot-isolation gates.

| Records | Pattern | Point reference ms | Current ms | Change |
|---:|---|---:|---:|---:|
| 8,192 | hits | 16.147 | 11.398 | -29.41% |
| 8,192 | mixed | 17.226 | 10.699 | -37.89% |
| 65,536 | hits | 31.021 | 25.769 | -16.93% |
| 65,536 | mixed | 25.561 | 20.588 | -19.46% |

The permanent `metadata-batch` suite compares the current API with the a0a8b89 algorithm: one blocking task and connection lock per API batch, and one cached point query per key. Both paths use the same reopened snapshot and alternate order each iteration. This is an algorithm comparison inside one executable, not a revision comparison.

The fixture has 8,192 or 65,536 structural leaf records, three namespaces, and 8-byte native IDs. Requests use a deterministic random order; mixed requests include misses and duplicates. Seed, inventory audit, correctness comparisons, and the commit used to check snapshot isolation are outside timing. API dispatch, key cloning, SQL reads, decoding, and output collection are timed. The inventory audit warms pages before both first and warm measurements.

Widths 1, 255, 256, 257, and 1,024 were measured in the first matrix; a second six-process matrix adds 127, 128, and 129. Together they cover the final standard and smoke width defaults. Every multi-key case improved against the reference. Single-key changes range from −1.47% to +4.31%, so there is no single-key speedup claim. Raw first/warm iterations and all width summaries are embedded in the JSON receipt.

At 8,192 records, hit-only current reads rise from 11.56 ms at width 128 to 14.84 ms at width 129 because the query pads to the next power of two. Width 129 still improves over the reference, but by 11.2% rather than 28.2%. Both sides of this cost boundary are permanent cases.

## Full fsck

Baseline is current main, `a0a8b8941feca5553478c32b2e03c5bd2bf68d7d`. The retained baseline executable was verified against the committed source receipt. Candidate and baseline use Rust 1.96.0, identical Cargo.lock, release optimization, CLI features, and package-only codegen-units=256. Fresh timings were collected for both binaries. Owned builds and tests had completed before these measurements.

The standard three-pair matrix contains 36 complete command executions. Reused packed fixtures were re-audited for exact root identity, revision, object/payload counts, checkout manifest, spill behavior, and cleanup. N−1 streams metadata and spills traversal sets; N caches metadata but spills sets; N+1 keeps both in memory.

| Records | Memory limit | Baseline s | Candidate s | Change | Baseline RSS MiB | Candidate RSS MiB |
|---:|---:|---:|---:|---:|---:|---:|
| 8,257 | 8,256 | 0.567 | 0.579 | +2.08% | 42.4 | 43.2 |
| 8,257 | 8,257 | 0.324 | 0.338 | +4.26% | 46.7 | 46.5 |
| 8,257 | 8,258 | 0.148 | 0.144 | -2.64% | 40.8 | 41.0 |
| 65,601 | 65,600 | 4.663 | 4.551 | -2.40% | 143.0 | 141.5 |
| 65,601 | 65,601 | 2.232 | 2.273 | +1.81% | 179.7 | 179.4 |
| 65,601 | 65,602 | 0.925 | 0.896 | -3.10% | 142.3 | 142.6 |

A five-pair repeat of the large fixture added 30 passing scans:

| Memory limit | Baseline s | Candidate s | Change |
|---:|---:|---:|---:|
| 65,600 | 4.868 | 4.705 | -3.33% |
| 65,601 | 2.640 | 2.289 | -13.31% |
| 65,602 | 0.922 | 0.933 | +1.14% |

The streamed target improved by 2.4% and 3.3% in the two runs. The cached controls bypass the changed method, yet varied by up to 13.3%. These shared-host measurements therefore do not establish a convincing full-fsck speedup. The change is retained for the clear direct batch-read improvement. No additional full-fsck benefit is claimed.

## Implementation and validation

One `VALUES` relation carries input ordinals and keys into a left join on the composite `(namespace, native_id)` index. At most 256 keys enter each query. Power-of-two widths limit the prepared cache to nine query shapes; NULL padding cannot match the non-null key. Ordinals restore caller order and duplicate positions without sorting. Missing keys stay `None`; the existing record decoder still validates compression, encoding, and key identity.

Turso EXPLAIN at widths 1, 2, 128, and 256 reports `SCAN requested` followed by `SEARCH objects USING INDEX sqlite_autoindex_objects_1 (namespace=? AND native_id=?) LEFT-JOIN`. A regression test requires the composite search and rejects an objects-table scan.

All 488 enabled library tests passed (18 ignored), as did all-features/all-targets Clippy, both online-collection integration tests, all 162 Python harness tests, formatting, and diff checks. Targeted tests additionally verify reuse of the same cached query after a decoding error leaves unread rows. Both `metadata-batch` and `fsck` passed through `benchmark all --profile smoke`; all six retained successful result files normalize for the dashboard.

## Excluded diagnostic runs

The first direct matrix and a reproduction hit Turso’s reader-slot ownership assertion during teardown, after emitting output. The harness rejected both failed processes. The final probe checks that the retained snapshot still misses an object committed later, releases that snapshot, then checks that a fresh snapshot sees the object. The corrected large fixture and all final benchmark processes pass. This is a benchmark lifecycle correction, not a fix for arbitrary overlapping-reader teardown. Failed output and the original executable hash are retained in the JSON receipt and excluded from summaries.

## Reproduction

Build candidate sources and the baseline revision with the recorded compiler and release profile. The JSON receipt includes the measured source patch, final source, Cargo artifact records, executable and lock hashes, fixture origin, raw results, and validation logs. The final post-measurement source change only strengthens a regression test; production code and the native benchmark body are verified identical to the measured build.

```console
cargo --config 'profile.release.package.casita.codegen-units=256' build -j 2 --release --locked --features cli --bin casita --message-format=json
cargo --config 'profile.release.package.casita.codegen-units=256' test -j 2 --release --locked --features cli --lib --no-run --message-format=json
benchmark run metadata-batch --profile standard --widths 1,255,256,257,1024 --repetitions 3 --probe-binary benchmarks/results/2026-09-09-metadata-bulk/casita-lib-test --output benchmarks/results/2026-09-09-metadata-bulk/direct.json
benchmark run metadata-batch --profile standard --widths 127,128,129 --repetitions 3 --probe-binary benchmarks/results/2026-09-09-metadata-bulk/casita-lib-test --output benchmarks/results/2026-09-09-metadata-bulk/padding.json
benchmark run fsck --profile standard --repetitions 3 --casita benchmarks/results/2026-09-09-metadata-bulk/candidate-casita --baseline-casita benchmarks/results/2026-09-09-metadata-bulk/baseline-casita --reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures --output benchmarks/results/2026-09-09-metadata-bulk/fsck.json
benchmark run fsck --profile standard --files 65536 --repetitions 5 --casita benchmarks/results/2026-09-09-metadata-bulk/candidate-casita --baseline-casita benchmarks/results/2026-09-09-metadata-bulk/baseline-casita --reuse-work-dir benchmarks/results/2026-09-08-fsck/paired-fixtures --output benchmarks/results/2026-09-09-metadata-bulk/focused-fsck.json
benchmark all --profile smoke --suites metadata-batch,fsck --repetitions 1 --bin-dir benchmarks/results/2026-09-09-metadata-bulk/smoke-bin --output benchmarks/results/2026-09-09-metadata-bulk/all-smoke
```

Use the executable paths emitted by Cargo; the local commands above use retained copies. Fresh fixtures can be generated by the existing fsck suite with `--seed-probe` instead of `--reuse-work-dir`. The suite is registered in the manifest, `benchmark all`, revision comparisons, and dashboard normalization.

## Integration before publication

Before publication, this change was rebased onto `ade46e004c58354e4ffbd959642a0eccd48ce4c7`
(checked metadata transactions and retained application reads). Both sets of
benchmark registrations are preserved. The integrated tree passed all **500**
enabled library tests, eight application/online-collection integration tests,
all-features/all-targets Clippy, all **168** benchmark-harness tests, formatting,
and diff checks. The `metadata-batch`, `metadata-primitives`, and `metadata-kv`
smoke suites passed together with debug binaries. These are correctness checks;
the performance measurements above retain their original a0a8b89 baseline and
were not rerun after this integration. Logs and the smoke ledger are embedded
in the JSON receipt.
