# Filesystem ingestion scheduling — 2026-09-11

Completed files now release admission slots immediately. The previous `buffered` stream held completed results behind the first unfinished file, leaving available work queued. The new `buffer_unordered` stream writes results into indexed page slots and publishes the page in the original post-order. Active ingestion remains bounded by the configured file concurrency; each page remains bounded to 1,024 entries. Defaults remain 16 files and 32 chunk uploads per writer. Errors and cancellation drop pending ingestion futures.

## Controlled scheduling result

The permanent `ingest-scheduling` suite compares both schedulers in one optimized test executable. It performs real rooted filesystem walks, reads and hashes; every sixteenth file is 64 KiB and others are 1 KiB. The skewed case adds a 20 ms wait to every sixteenth file and 1 ms to others. This models uneven readiness; it is not measured remote-storage latency or complete repository ingestion. Fixture setup and validation are outside timing. Case order is randomized with seed 1729; medians below use three repetitions.

| Files | Ordered, ms | Ready, ms | Speedup at concurrency 16 |
|---:|---:|---:|---:|
| 15 | 22.67 | 22.20 | 1.02× |
| 16 | 22.51 | 22.66 | 0.99× |
| 17 | 44.46 | 28.90 | 1.54× |
| 64 | 87.32 | 33.96 | 2.57× |
| 1,023 | 1407.18 | 271.89 | 5.18× |
| 1,024 | 1393.03 | 264.96 | 5.26× |
| 1,025 | 1438.98 | 292.16 | 4.93× |

The improvement appears once work exceeds the 16-file window and remains across the page boundary. The root directory occupies one page entry, so 1,023 files fill one page and 1,024 files require another. The full corpus includes serial and concurrency-32 controls, plus a uniform case without injected waits.

At 1,024 files and concurrency 16, the uniform median was 56.94 → 55.74 ms. The initial serial uniform samples contained large off-CPU outliers (all retained). A focused randomized nine-repetition repeat measured 114.12 → 115.19 ms, with ranges 107.17–125.90 and 109.10–119.69 ms. There is no persistent serial slowdown in that repeat.

## Complete repository imports

Release CLI baseline: `898257bb94837c59d71b579c8803862dbbf2c4c7`. Candidate: the production changes accompanying this report. The baseline and candidate each completed 45 fresh durable imports: five corpora × file concurrency 1/16/32 × three repetitions, with chunk concurrency fixed at 32. Timing excludes input generation, repository initialization, checkout and fsck. Sources are warm in the OS cache. Both batches ran on a shared Ryzen 7 7840S development host with Btrfs; this investigation launched no builds during measurements.

| Corpus | File concurrency | Ordered, s | Ready, s |
|---|---:|---:|---:|
| tiny | 1 | 64.619 | 64.331 |
| tiny | 16 | 21.543 | 21.326 |
| tiny | 32 | 11.934 | 12.300 |
| below-chunker-minimum | 1 | 4.745 | 4.002 |
| below-chunker-minimum | 16 | 0.696 | 0.573 |
| below-chunker-minimum | 32 | 0.414 | 0.396 |
| above-chunker-minimum | 1 | 3.890 | 2.372 |
| above-chunker-minimum | 16 | 0.979 | 0.771 |
| above-chunker-minimum | 32 | 0.608 | 0.456 |
| large | 1 | 7.385 | 5.643 |
| large | 16 | 6.088 | 4.871 |
| large | 32 | 5.173 | 5.166 |
| mixed | 1 | 15.913 | 9.263 |
| mixed | 16 | 4.354 | 2.974 |
| mixed | 32 | 3.266 | 2.831 |

These batches ran in separate phases. Substantial apparent gains in serial controls show host variation, so the table does not establish a broad end-to-end speedup. Tiny-file throughput was approximately unchanged. RSS varies by corpus and concurrency; unchanged bounds do not imply identical measured memory use.

To reduce phase bias, five additional mixed-corpus pairs alternated execution order: ordered/ready, then ready/ordered. Each imports 512 files: 32 × 4 MiB plus 480 × 1 KiB, at file concurrency 16 and chunk concurrency 32.

| Pair | Execution order | Ordered, s | Ready, s | Ordered / ready |
|---:|---|---:|---:|---:|
| 0 | ordered, ready | 4.152 | 1.425 | 2.91× |
| 1 | ready, ordered | 2.401 | 1.389 | 1.73× |
| 2 | ordered, ready | 2.570 | 2.033 | 1.26× |
| 3 | ready, ordered | 3.097 | 2.163 | 1.43× |
| 4 | ordered, ready | 4.272 | 4.954 | 0.86× |

The paired medians were 3.097 → 2.033 s (1.52× ratio of medians). Four pairs improved and the final pair regressed. This supports a mixed-size benefit on this host but remains noisy; the controlled scheduling fixture provides the strongest causal evidence. Storage and metadata contention still limit full imports.

## Correctness and retained artifacts

All 100 CLI imports passed reopened-root identity, exact checkout manifest and fsck gates. Roots also match between baseline and candidate for every corpus, including all paired repeats. All 252 main scheduling samples and 18 focused serial samples passed exact path order, size and digest checks, active-read bounds and page bounds. Rust regression tests additionally gate blocked-first-file admission, delayed page publication, errors, cancellation, cached-file reuse, excluded paths, directories and symlinks across pages.

- [Baseline raw results](ordered.json) and [table](ordered.md).
- [Candidate raw results](ready.json) and [table](ready.md).
- [Scheduling raw results and process output](scheduling.json) and [table](scheduling.md).
- [Serial-control repeat](serial-control.json) and [table](serial-control.md).
- [Exact alternating commands](paired-execution.json); each command names its retained `paired-*.json` result.

Binary SHA-256 identities:

| Artifact | SHA-256 |
|---|---|
| Ordered CLI | `49bac535d4517f59e4ebbb15469218b1bea1c8d8d1f7a6ba54f7625d022fd8c2` |
| Ready CLI | `29a3f1fae15ac1a39da5e81cca16eb7c8f909c07c237f6da59017bac7df26c68` |
| Scheduling test executable | `a5b16c358546ad58608e75e8e2d2ae2d5a9f9d8fc3bd83041b573fc0f08b78d1` |

The scheduling test executable predates only equivalent style cleanups (`is_multiple_of` and borrowed path comparisons) in its test source. The measured candidate contains the scheduling change on baseline `898257b`. Before publication, this change was rebased onto `843497a`, which also includes shared metadata pages and FastCDC 5. The end-to-end numbers above therefore do not measure that combined revision. The test-only ordered reference uses the same indexed collector as the candidate, isolating admission policy; the baseline CLI uses the unmodified previous implementation.

## Reproduction

Validation passed: `cargo clippy --all-features --all-targets -- -D warnings`,
the four release filesystem tests (with the benchmark ignored),
`repository::tests::filesystem_import_checkout_and_reimport_preserve_identity`,
25 Python harness/registration tests, targeted Rust formatting, and
`git diff --check`.

Run commands inside the project development shell. Preserve release CLI binaries before switching builds. Build the baseline from revision `898257b` in a separate checkout; run the current permanent Python harness against both binaries. No local temporary binary paths are required by the suites:

```sh
cargo build --release --features cli --bin casita
python3 -m benchmarks.cli run ingest-concurrency --profile standard \
  --file-concurrency 1,16,32 --chunk-concurrency 32 --repetitions 3 \
  --no-build --casita-bin /absolute/path/to/ordered-cli \
  --output /tmp/ordered.json --report /tmp/ordered.md
python3 -m benchmarks.cli run ingest-concurrency --profile standard \
  --file-concurrency 1,16,32 --chunk-concurrency 32 --repetitions 3 \
  --no-build --casita-bin /absolute/path/to/ready-cli \
  --output /tmp/ready.json --report /tmp/ready.md
python3 -m benchmarks.cli run ingest-scheduling --profile standard \
  --repetitions 3 --output /tmp/scheduling.json
python3 -m benchmarks.cli run ingest-scheduling --profile standard \
  --counts 1024 --concurrency 1 --patterns uniform --repetitions 9 \
  --output /tmp/serial-control.json
```

For the alternating repeat, run `ingest-concurrency` with `--corpora mixed --file-concurrency 16 --chunk-concurrency 32 --repetitions 1`, following the ten-command order in `paired-execution.json` and substituting the binary and output paths. Scheduling can reuse a release lib-test executable with `--probe-binary PATH --no-build`.

Both suites are registered in `benchmarks/manifest.json`, `benchmark all`, and revision comparisons. The permanent cases cover both sides of the admission window, page boundary and existing 128 KiB chunking threshold. No defaults were tuned from these noisy measurements.
