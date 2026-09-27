# Bounded small-file ingestion spike

**Do not promote the spike globally yet.** Batching rooted opens and small-file
reads, then staging bytes directly, improves small-file import and serial hello
sandbox overhead. Concurrent real jq waves leave a throughput/CPU concern that
must be separated from input-store preparation before changing the default.
Production ingestion is unchanged; source patches and benchmarks are retained.

[Full Obrador investigation and all real-build samples](https://github.com/cachix/obrador/tree/main/benchmarks/small-file-import-2026-09-13).
Hello's paired full-call median improves 2.514 ms (4/4 pairs); jq is effectively
flat (2/4). Concurrent jq waves are slower in 3/4 pairs, with paired medians
+1.353 s wave time and +211.721 ms SDK CPU. This does not establish that output
batching causes the regression: the harness also uses the modified importer to
prepare inputs before timing. The 10 ms full-cycle target remains unmet.

## Implementation

The first [read-batch spike](read-batch.patch) does the rooted no-follow open,
regular-file check and bounded small-file reads in one blocking task. A size
hint at or below 256 KiB permits an eager read through EOF, capped at 256 KiB + 1.
If a file grows past the window, its prefix and remaining open handle continue
through the existing streaming importer. Files larger than the window remain
streamed. The fstat size is a hint, never a payload truncation boundary.

The final [buffered-stage spike](buffered-stage.patch) also sends complete
buffers through the existing verified `stage_blob` API. This avoids allocating
another 64 KiB buffer in `stage_blob_reader`. Both paths keep digest and payload
limits, executable metadata, root containment and non-destructive reads.
Each patch applies independently to `4788d7e` with `git apply --unidiff-zero`.
Neither patch is enabled in production.

## Permanent benchmark

`benches/filesystem_import.rs` is registered in `benchmarks/manifest.json` under
`core-primitives` and included in `benchmark all`. It imports deterministic,
uncompressed filesystem data into a fresh memory repository, matching the
storage kind used by the Obrador closure harness. The timer includes import and
publication, excluding fixture setup, verification and repository destruction.
Every iteration verifies an independently constructed canonical root (including
executable bits), published root, and complete readback of every payload.

The measurements use two ABBA series, four processes per variant. Each process
runs all 12 cases, with a 0.2 s warmup and 0.5 s requested measurement window,
10 Criterion samples per case. An initial candidate latency spike prompted the
second full series. Nothing was discarded. The table reports the median of
four process medians, in milliseconds; these are not independent per-iteration
samples. Above-threshold differences vary with scheduling; no intended large-file
optimization is claimed.

| File size / count / concurrency | Baseline, ms | Candidate, ms | Change |
| --- | ---: | ---: | ---: |
| bytes-0-files-48/1 | 2.054 | 1.347 | -34.4% |
| bytes-0-files-48/16 | 0.804 | 0.498 | -38.1% |
| bytes-1024-files-48/1 | 3.608 | 1.826 | -49.4% |
| bytes-1024-files-48/16 | 1.040 | 0.752 | -27.7% |
| bytes-1048576-files-16/1 | 18.884 | 19.398 | +2.7% |
| bytes-1048576-files-16/16 | 25.246 | 23.496 | -6.9% |
| bytes-262143-files-16/1 | 5.995 | 4.777 | -20.3% |
| bytes-262143-files-16/16 | 8.176 | 5.612 | -31.4% |
| bytes-262144-files-16/1 | 5.810 | 4.709 | -19.0% |
| bytes-262144-files-16/16 | 8.056 | 5.573 | -30.8% |
| bytes-262145-files-16/1 | 6.237 | 5.795 | -7.1% |
| bytes-262145-files-16/16 | 8.908 | 9.006 | +1.1% |

The candidate's 1 KiB serial process medians range 1.794–6.838 ms; baseline
ranges 3.213–6.110 ms. Those spikes remain visible in the
[complete threshold distributions](threshold-summary.json).
Process peak RSS, including fixtures and readback across all cases, ranges
83.33–110.51 MiB baseline and 83.82–91.53 MiB candidate. This is not an isolated
import allocation measure or a memory-saving guarantee. Eager contents are
bounded per file; Vec allocation capacity may exceed the logical read window.

[Initial commands, CPU/RSS and samples](threshold/results.json),
[repeat commands, CPU/RSS and samples](threshold-repeat/results.json).
Raw Criterion estimates and sample arrays are retained beside the run logs.
Process CPU includes untimed setup/readback and cannot be treated as import CPU.
Obrador SDK CPU is reported separately in its real/replay study.

## Reproduce

Use Casita `4788d7e` with this benchmark file and its Cargo `[[bench]]` registration
for both baseline and candidate. Copy the retained `Cargo.lock` into each
worktree before building. The benchmark file is identical in both binaries;
apply only `buffered-stage.patch` to the candidate. Preserve each executable
before rebuilding the other:

```sh
cargo bench --locked --features experimental --bench filesystem_import --no-run --message-format=json
python3 benchmarks/reports/2026-09-13-small-file-import/compare.py \
  --baseline /path/to/baseline --candidate /path/to/candidate \
  --output /tmp/new-filesystem-comparison
```

The comparison script is a POSIX wrapper for the registered Criterion corpus.
It records exact commands, hashes, all samples and process CPU/RSS. Repeat with
a new output directory; it refuses to overwrite results. The default corpus
also remains runnable with `cargo bench --features experimental --bench
filesystem_import` and through `benchmark all` on current main.

[Binary identities](artifacts.json), [final spike source](source.json),
[initial source](initial-source.json), [initial full tests](tests.log),
[initial Clippy](clippy.log), [final-spike root tests](buffered-root-tests.log),
[final-spike filesystem tests](buffered-import-tests.log),
[final-spike Clippy](buffered-clippy.log), [harness tests](harness-tests.log).
The initial spike passed 615 tests (33 ignored); final byte-slice staging passed
12 root and 13 filesystem tests (one ignored) and all-feature/all-target Clippy.
All work ran as UID 1000 without root help or host configuration changes.

The benchmark was integrated onto Casita main after the experiment. Measured
binaries remain on the documented `4788d7e` base; no timings from newer main are
substituted into this comparison. Production source changes were removed.

Final benchmark integration passed all 12 cases in smoke mode, eight corpus
harness tests, all-feature/all-target Clippy, and formatting:
[benchmark smoke](final-bench-smoke.log), [harness](final-harness-tests.log),
[Clippy](final-clippy.log). No production source differs from the target main.

[Compiler provenance](toolchain.json): measured executables used Rust 1.95.0
from Obrador `218a25c`'s locked devenv shell. Casita's final commit hook also
passed its pinned Rust 1.96.0 all-feature/all-target Clippy check.
