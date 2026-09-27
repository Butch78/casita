# Validation-preserving current metadata reads

The short transaction now uses the exact same repository-state query and decoding
helper as retained snapshots. Missing state, malformed revision width/type,
negative generation, and invalid catalog-reference type remain errors, including
for empty requests. State errors still take precedence over invalid request
errors; invalid requests are checked before copying their keys.

This supersedes the [earlier measurement](../2026-09-21-current-metadata-reads/README.md),
which omitted state validation from the short path. The remaining optimization
combines transaction acquisition, state validation, record reads, and cleanup in
one blocking worker using the existing pool. No corruption check is removed from
the snapshot-opening state query.

## Fresh paired Linux results

Five fresh processes per record count, 20 iterations each. Each iteration
alternates the order of short current reads and snapshot-opening reads. Both use
the same executable, state validation, keys, values, and eight-idle-connection pool.
The table reports the median of five process medians in microseconds per complete
concurrent burst.

| Records | Concurrent reads | Snapshot path | Short transaction | Ratio |
|---:|---:|---:|---:|---:|
| 257 | 1 | 66.5 | 32.7 | 2.04x |
| 257 | 7 | 67.1 | 39.8 | 1.69x |
| 257 | 8 | 74.8 | 38.6 | 1.94x |
| 257 | 9 | 168.6 | 121.7 | 1.39x |
| 257 | 15 | 318.2 | 238.1 | 1.34x |
| 257 | 16 | 333.6 | 230.9 | 1.44x |
| 257 | 17 | 355.6 | 242.8 | 1.46x |
| 8,192 | 1 | 68.3 | 34.4 | 1.98x |
| 8,192 | 7 | 68.3 | 41.6 | 1.64x |
| 8,192 | 8 | 76.0 | 39.2 | 1.94x |
| 8,192 | 9 | 172.2 | 130.0 | 1.32x |
| 8,192 | 15 | 322.4 | 252.9 | 1.27x |
| 8,192 | 16 | 348.5 | 264.2 | 1.32x |
| 8,192 | 17 | 366.9 | 253.5 | 1.45x |

Every count/width improves in each of its five paired process medians. At 8,192
records, the single-key saving is about 34 microseconds. The eight-to-nine
concurrency cliff remains; the pool limit is unchanged. The larger bursts improve
by about 1.27–1.46x at widths 9/15/16/17.

These are warm local microbenchmarks on a shared Linux host. Our library tests and
release compilation completed before measurement; other activity on the host was
not controlled. Absolute timings differ from the earlier run and should not be
subtracted across runs to estimate validation cost. This compares the two paths
within each fresh process. There is no end-to-end derivation speedup or fresh
macOS/Windows performance claim. The fixture varies application-record count,
with one payload and 257 reverse references, not payload-catalog size.

## Evidence and reproduction

`source.json` identifies the exact candidate commit and source hashes.
`measurements.json.gz` retains ten processes, 290 operation summaries, per-iteration
samples, environment, executable fingerprint, and correctness gates. `Cargo.lock.gz`
retains the dependency resolution. The original files are compressed losslessly;
read them with `gzip -dc`. The adjacent Markdown summary remains readable.

The permanent `metadata-kv` entry and `benchmark all` include widths 1/7/8/9/15/16/17.
The selected all-suite smoke run covers 256/257/8,191/8,192 records and batches 1/16,
including exact values, scans, commits, reopen, and GC gates. Its raw outputs,
artifact identities, and execution ledger are retained in `corpus/`.

```sh
gzip -dc benchmarks/reports/2026-09-21-validated-metadata-reads/Cargo.lock.gz > Cargo.lock
devenv shell benchmark run metadata-kv --counts 257,8192 --batches 1 \
  --iterations 20 --repetitions 5 --output /tmp/validated-metadata-reads.json
devenv shell benchmark all --suites metadata-kv --profile smoke \
  --repetitions 1 --output /tmp/validated-metadata-reads-all
```

The retained run reused a release library test executable built with
`cargo test --release --features cli --lib --no-run`. It supplied `--probe-binary`
to the measurement adapter and `--bin-dir` to the corpus runner. Their ledgers
retain the exact commands and binary fingerprints. Commands without these
overrides rebuild the artifacts. The toolchain is the pinned devenv Rust 1.96.0.

## Validation and outstanding checks

- Fresh all-features library run: 865 passed, 48 ignored, zero failures.
- The corruption regression covers five damaged-state fixtures, each with empty,
  valid, and oversized requests, comparing error type and message to snapshots.
- All nine application-record tests and database cleanup/cancellation tests pass
  as part of that library run.
- Fresh all-features/all-target Clippy with warnings denied and formatting pass.
- 336 Python tests passed after the benchmark report-text change, before the final
  Rust-only refinement; no Python code changed afterward.
- Ten measured processes and eight corpus smoke processes passed their gates.

The earlier interrupted runs were stopped at the user's request, then validation
and measurement resumed. The complete integration/default-feature suites have not
been rerun here. The previously recorded `verified_cli` fixture failure and
interrupted default/baseline checks remain unresolved. Keep the PR draft until
those gaps are resolved. Earlier validation logs and limitations remain in the
superseded report; this report does not treat those older runs as fresh checks.
