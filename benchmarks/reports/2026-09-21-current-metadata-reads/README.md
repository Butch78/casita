# Short current metadata reads

**Superseded:** these measurements omitted the snapshot state-row validation
on the short path. They are not an equivalent-behavior speedup claim. See
[the validation-preserving rerun](../2026-09-21-validated-metadata-reads/README.md).

This extracts the production record-read optimization from PR #82 onto
`c038bc2` without its inline-storage experiments or dependency on PR #81.
Current main already pools snapshot connections and stores an external catalog
reference. The remaining avoidable work is the state-row query and two extra
worker dispatches before reading application records.

`Repository::get` now performs one short query-only transaction on the existing
pool. Rollback completes before returning the connection or replying. A detached
worker owns cleanup after caller cancellation. Retained snapshots keep their
existing behavior; other backends use the protected snapshot fallback.

## Paired Linux results

Five fresh processes per record count, 20 iterations per process. Each iteration
alternates the order of current reads and the existing snapshot-opening path.
Both paths use the same optimized executable, keys, values, and eight-idle
connection pool. Each timing is for the entire concurrent burst. The table gives
the median of five process medians, in microseconds.

| Records | Concurrent reads | Snapshot path | Short transaction | Ratio |
|---:|---:|---:|---:|---:|
| 257 | 1 | 111.6 | 55.8 | 2.00x |
| 257 | 7 | 109.8 | 70.6 | 1.55x |
| 257 | 8 | 116.4 | 58.2 | 2.00x |
| 257 | 9 | 296.8 | 140.1 | 2.12x |
| 257 | 15 | 512.3 | 181.7 | 2.82x |
| 257 | 16 | 540.8 | 179.3 | 3.02x |
| 257 | 17 | 562.2 | 181.1 | 3.10x |
| 8,192 | 1 | 138.3 | 65.9 | 2.10x |
| 8,192 | 7 | 153.2 | 76.6 | 2.00x |
| 8,192 | 8 | 136.5 | 68.3 | 2.00x |
| 8,192 | 9 | 350.8 | 133.0 | 2.64x |
| 8,192 | 15 | 537.8 | 201.8 | 2.66x |
| 8,192 | 16 | 570.5 | 217.3 | 2.63x |
| 8,192 | 17 | 595.4 | 200.1 | 2.98x |

Every count/width improves in each of the five paired process medians.
The eight-to-nine concurrency step still increases both paths' cost. The pool
limit stays at eight; 15/16/17 retain the original investigation's historical
sixteen-idle boundary cases. The permanent manifest entry and `benchmark all`
include all seven concurrency widths.

These are warm local microbenchmarks, not derivation throughput or isolated
latency floors. Other builds were active on the shared host. There is no fresh
macOS measurement. The workload varies application-record count, with one
payload and a fixed 257-record reverse-reference fixture; it does not establish
scaling with a large payload catalog. Earlier PR #82 ratios are not reused.

## Evidence and reproduction

`measurements.json.gz` retains all ten process outputs, executable SHA-256,
environment, 290 operation summaries, and all per-iteration samples. The corpus
smoke ledger and raw output are in `corpus/`; it completed all eight combinations
of 256/257/8,191/8,192 records and batches of 1/16, including all concurrency
widths and exact value, scan, commit, reopen, and GC correctness gates.
`source.json` identifies the base and exact source hashes. `Cargo.lock.gz`
retains the dependency resolution used by both builds; the repository normally
ignores this generated lockfile.

Large JSON captures are compressed losslessly after the run. The original
filenames in command ledgers refer to the uncompressed captures. Read them with
`gzip -dc`; the adjacent Markdown summaries remain directly readable.

From an isolated checkout of this change:

```sh
gzip -dc benchmarks/reports/2026-09-21-current-metadata-reads/Cargo.lock.gz > Cargo.lock
devenv shell benchmark run metadata-kv --counts 257,8192 --batches 1 \
  --iterations 20 --repetitions 5 --output /tmp/current-metadata-reads.json
devenv shell benchmark all --suites metadata-kv --profile smoke \
  --repetitions 1 --output /tmp/current-metadata-reads-all
```

The retained run supplied `--probe-binary` to reuse the release library test
executable built with `cargo test --release --features cli --lib --no-run`.
The corpus run reused that same binary via `--bin-dir`; its exact command and
binary fingerprint are recorded in `corpus/execution.json` and
`corpus/artifacts.json`. Without these overrides the adapters rebuild artifacts.

## Validation and outstanding checks

- All-features library: 864 passed, 48 ignored, no failures. The complete
  `cargo test --all-features` invocation subsequently failed in
  `verified_cat_local_and_transfer_source_emit_only_authenticated_bytes` at
  `tests/verified_cli.rs:52`, which tries to read a nonexistent loose
  `blobs/bao/b3/...` proof file. Current storage packs Bao roots. This test and
  the storage layout are unchanged by this extraction. A clean-baseline runtime
  reproduction is **not confirmed**: both baseline compiler attempts at
  `c038bc2` were terminated by SIGTERM before the test could run.
- Focused database/snapshot tests: 32 passed, three benchmark probes ignored.
  All nine application-record regression tests passed, including ordered
  duplicates/misses and empty reads through the new path, 16/17 MiB result
  limits, snapshot consistency, and foreign-handle updates.
- `cargo test --no-default-features` passed. All-features doctests passed.
- All-features/all-targets Clippy passed with warnings denied. Formatting and
  whitespace checks passed. Rust documentation passed with warnings denied.
- Python benchmark tests: 336 passed. The ten measured processes and eight
  selected `benchmark all` smoke processes passed their complete correctness
  gates. These are the selected metadata suite, not a run of every benchmark.
- The full default-feature test process and its retry were both terminated by
  SIGTERM. Neither completed, so default-feature validation remains incomplete.
  Logs preserve both interruptions. No fresh macOS or Windows run was performed.

The PR remains a draft. Resolve the CLI fixture failure and finish the interrupted
checks on a stable runner before merging. The measured metadata-read source did
not change after capture. Compressed validation logs retain the passing checks,
the CLI failure, and all interrupted attempts.
