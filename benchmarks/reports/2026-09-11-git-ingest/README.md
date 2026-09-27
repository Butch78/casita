# Bounded Git ingestion — 2026-09-11

Git import now overlaps verified object staging within two positive limits: 16 active objects and 64 MiB of decoded source bytes by default. The CLI exposes `--git-concurrency` and `--git-max-buffered-bytes`; Rust exposes `GitImport::with_concurrency`, `with_max_buffered_bytes` and the corresponding experimental `NativeGitImportOptions` fields; IPC accepts `options.concurrency` and `options.max_buffered_bytes`. Concurrency 1 selects serial staging.

An object larger than the byte budget runs alone. With active work, the importer checks the next object header before decoding its body and keeps only one pending key. Completed bodies are released instead of accumulating in the Gix buffer pool. The budget excludes Gix caches, delta-decoding workspace, metadata and payload-store buffers; it is not an RSS limit. Source decoding remains synchronous, so this change overlaps asynchronous staging rather than parallelizing Git decompression across cores.

Incremental imports preserve parent-first discovery before admitting queued trees, allowing reuse of the previous exact closure even when old source objects are absent. Publication checkpoints drain active writes before flushing storage: a flush must not wait behind an unfinished writer whose future has stopped being polled. Completed seals remain bounded by the staging window and are published within the existing object/link batch limits. Errors and cancellation drop outstanding work and preserve the previously rooted view.

## Paired end-to-end result

The final comparison alternated the original serial release binary (`4be9276`) and the candidate with concurrency 16 and a 64 MiB budget. Five pairs used the permanent loose-object fixture: 64 files, including one 64 KiB file per sixteen files and 1 KiB for the rest. Initial import contains 67 reachable native objects; the fast-forward modifies every fourth file and extends the closure to 86 objects. Native-pack caching was disabled. All five pairs improved for both operations.

| Pair | Execution order | Initial old, s | Initial new, s | Incremental old, s | Incremental new, s |
|---:|---|---:|---:|---:|---:|
| 0 | old, new | 1.4319 | 0.5133 | 0.6893 | 0.3143 |
| 1 | new, old | 1.5172 | 0.5239 | 0.7037 | 0.2680 |
| 2 | old, new | 1.5765 | 0.5400 | 0.7392 | 0.3287 |
| 3 | new, old | 1.5008 | 0.5046 | 0.7184 | 0.3330 |
| 4 | old, new | 1.4866 | 0.5371 | 0.7568 | 0.3314 |

Ratios of medians: initial **1.501 → 0.524 s (2.86×)**; incremental **0.718 → 0.329 s (2.19×)**. These are local warm-cache results on a shared development host, not expected rates for every repository or backend.

## Controlled upload latency

The permanent scheduling fixture runs the real importer against an instrumented memory payload store. It compares concurrency 1 and 16 in the same optimized executable, with either no injected delay or 5 ms per upload. It deliberately uses seven-object publication batches to exercise checkpoint flushing with a larger staging window. Source construction, repacking and independent Git inventory/closure validation are outside timing. Each table cell is a median of three repetitions at 64 files and the 64 MiB budget.

| Source | Upload delay | Operation | Serial, ms | Concurrent, ms | Speedup |
|---|---:|---|---:|---:|---:|
| Loose | 0 ms | initial-import | 4.062 | 4.691 | 0.87× |
| Loose | 0 ms | incremental-import | 2.263 | 2.407 | 0.94× |
| Loose | 5 ms | initial-import | 418.064 | 66.723 | 6.27× |
| Loose | 5 ms | incremental-import | 120.054 | 27.501 | 4.37× |
| Packed | 0 ms | initial-import | 3.187 | 3.213 | 0.99× |
| Packed | 0 ms | incremental-import | 2.180 | 2.040 | 1.07× |
| Packed | 5 ms | initial-import | 415.244 | 65.774 | 6.31× |
| Packed | 5 ms | incremental-import | 119.195 | 28.023 | 4.25× |

With injected latency, initial import improves about 6.3× and incremental import about 4.3×. Without injected waits, the loose-source initial case costs about 0.63 ms more; packed-source medians are approximately unchanged. Header inspection and scheduling have overhead when uploads complete immediately. The delay is a controlled fixture, not a measurement of a network service.

## Window and byte-budget boundaries

Both sides of the 16-object window and 64 KiB fixture-object size are permanent cases. The following initial-import medians use packed sources, concurrency 16 and 5 ms upload delay. Below 64 KiB, each large object must run exclusively; the nearby budgets still cannot fit that object alongside a 1 KiB file. The larger budget permits wider overlap.

| Files | 65,535-byte budget, ms | 65,536-byte budget, ms | 65,537-byte budget, ms | 64 MiB budget, ms |
|---:|---:|---:|---:|---:|
| 15 | 38.41 | 38.48 | 38.65 | 27.03 |
| 16 | 38.49 | 38.90 | 38.78 | 26.64 |
| 17 | 44.65 | 45.14 | 44.89 | 32.87 |
| 64 | 89.97 | 89.38 | 90.36 | 65.77 |

## Exploratory CLI sweep and host variation

The baseline completed 18 imports. The candidate completed 144 imports across loose, packed-without-deltas and delta-heavy sources, concurrency 1/16, four byte budgets and three repetitions. The initial sweep had large timing drift: identical serial configurations varied severalfold. In particular, its three default-budget loose-source concurrent samples appeared slower than its serial samples. All original samples are retained.

A focused nine-repetition same-binary repeat resolved that apparent regression: initial import medians were 1.515 → 0.523 s and incremental medians 0.706 → 0.322 s. The final alternating old/new comparison above then confirmed the improvement against the original implementation under the later host conditions. The original baseline was substantially faster in absolute time than the later serial runs, so unpaired cross-phase ratios are unsuitable as performance claims.

All measurements used warm source caches on the shared Ryzen 7 7840S/Btrfs development host. This investigation launched no overlapping builds during timed runs, but unrelated host activity remained. Process RSS is retained; the source-byte budget does not bound total process memory. These results do not justify claiming faster CPU-only decoding or a universal throughput multiplier.

## Correctness and validation

- **218 CLI imports** passed source object-count, reopened view/ref, exact checkout and fsck gates. Root identities also match across both executables and every concurrency/byte-budget setting; loose and packed representations of the same fixture select identical views.
- **768 controlled import samples** passed independent `git rev-list`/`git cat-file` inventory equality, complete closure verification, active-upload/byte bounds, oversized-object exclusivity and zero-active-upload assertions before checkpoint flushes.
- Six release Git repository tests passed, including unchanged imports, missing old source objects, generic transfer, failure/cancellation and resource-bound cases; the release Git IPC test passed, including zero-limit rejection.
- All-feature/all-target Clippy with warnings denied passed. The 25 Python harness/registration tests passed. The built CLI exposes both flags and rejects zero. Documentation build/link validation and targeted Rust formatting passed.

Artifacts:

- [Original serial CLI sweep](serial.json), [table](serial.md).
- [Candidate CLI sweep](concurrent.json), [table](concurrent.md).
- [Focused loose-source repeat](loose-repeat.json), [table](loose-repeat.md).
- [Controlled scheduling samples and process output](scheduling.json), [table](scheduling.md).
- [Exact alternating execution commands](paired-execution.json), with their `paired-*.json` outputs alongside this report.

| Executable | SHA-256 |
|---|---|
| Original CLI | `ee44cc36e08a22822a5b5c81a095efb1cf528c57da69056c44a6cae29c6fad64` |
| Candidate CLI | `b1b442c6339625fb182bd61cddc3dbebf2bcec7e399488b2308b82d372f1fffc` |
| Scheduling probe | `10d7266ab57903fe0f430415786d866c6f9de142bc2d6226ecb65b03843a724f` |

## Reproduction

Run in the development shell. Build the original CLI from `4be9276` in a separate checkout with `cargo build --release --features cli,git --bin casita`, and preserve it at an absolute path. Build the candidate with the same flags. The current permanent harness can run either executable:

```sh
python3 -m benchmarks.cli run git-ingest-concurrency --profile standard \
  --counts 64 --concurrency 1 --max-buffered-bytes 67108864 --omit-limits \
  --repetitions 3 --no-build --casita-bin /absolute/path/to/original-cli \
  --output /tmp/serial.json --report /tmp/serial.md
python3 -m benchmarks.cli run git-ingest-concurrency --profile standard \
  --counts 64 --repetitions 3 --no-build --casita-bin /absolute/path/to/candidate-cli \
  --output /tmp/concurrent.json --report /tmp/concurrent.md
python3 -m benchmarks.cli run git-ingest-scheduling --profile standard \
  --repetitions 3 --output /tmp/scheduling.json --report /tmp/scheduling.md
python3 -m benchmarks.cli run git-ingest-concurrency --profile standard \
  --counts 64 --layouts loose --concurrency 1,16 --max-buffered-bytes 67108864 \
  --repetitions 9 --no-build --casita-bin /absolute/path/to/candidate-cli \
  --output /tmp/loose-repeat.json --report /tmp/loose-repeat.md
```

For the final paired comparison, follow `paired-execution.json` in order, substituting local executable and output paths. Each invocation runs one initial/incremental pair. The scheduling suite can reuse a release lib-test binary built with `cli,git` through `--probe-binary PATH --no-build`. Both suites are registered in `benchmarks/manifest.json`, included in `benchmark all`, and support revision comparisons.
