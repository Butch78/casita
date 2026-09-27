# Investigation of cached and first-read costs

The approximately 69 ms cached result is not a lower bound. It includes opening
and closing 2,055 files, Python file wrappers, reading, verification, and timing.
The added controls separate these costs on Linux. They do not measure macOS.

## Diagnostic results

`linux-read-controls-fixed.json` completed three standard repetitions, 216
mounted/host samples, and twelve direct-reader passes, with all listed security
and correctness gates passing. Cached controls run three trials per repetition;
the following are medians of all nine trials in milliseconds. Ranges show the
substantial shared-host variation, not confidence intervals.

| Cached workload | Host median (range) | Linux FUSE median (range) |
| --- | ---: | ---: |
| pathlib read/verify, 2,055 files | 28.58 (23.23–49.39) | 74.72 (61.53–152.79) |
| open/close, same 2,055 files | 15.69 (13.86–19.15) | 45.98 (41.89–109.58) |
| POSIX open/pread/close/verify, same files | 21.03 (18.09–27.11) | 57.17 (49.71–127.91) |
| SHA-256 over the fixture in client memory | 8.90 (7.45–10.94) | 8.89 (8.36–12.20) |
| One file reopened/read/verified 2,055 times | 18.21 (15.76–24.75) | 50.23 (44.97–115.32) |
| One held descriptor read/verified 2,055 times | 10.56 (9.51–11.15) | 10.20 (9.18–13.67) |

Every cached pathname-read trial made 2,055 FUSE open calls and zero FUSE read,
lookup, or getattr calls. The held-descriptor and memory controls made zero of
all four. This attributes much of the cached gap to repeated opens, rather than
repository fetches or a limit on cached byte access. It does not establish an
exact additive decomposition: workload overhead, scheduling, and load vary.

The fixed mounted first-read workload took 14.14, 8.16, and 7.89 seconds. The
timed `ContentReader::open_blob` calls account for 12.64, 7.56, and 7.35 seconds
respectively. In repetitions 1 and 2 the per-file median in successive 256-file
groups stays around 3.4–3.8 ms, replacing the original steady 3-to-12 ms growth.
Repetition 0 was particularly affected by load and is retained in the report.

Direct per-object repository reads took 7.59–25.64 seconds per 2,055-file pass;
almost all that time was in open. A shared retained session took 79–884 ms, with
four of six passes around 79–81 ms. That is evidence that per-object admission
is an important remaining cost. It is not a claim that a mounted filesystem
would achieve the direct Rust loop's time or that retaining a whole repository
snapshot indefinitely is acceptable.

The next cached-read experiment is avoiding repeated open round trips while
preserving bounded reader retention. Linux supports
[zero-message opens](https://docs.rs/fuse-backend-rs/0.14.0/fuse_backend_rs/abi/fuse_abi/struct.FsOptions.html#associatedconstant.ZERO_MESSAGE_OPEN),
but our current per-open reader design depends on open/release handles. This
requires a reader-lifetime design and validation of uncached/large/concurrent
reads and collection; it is not a safe one-flag production change. For first
reads, investigate reusing admission work with protection scoped to the mounted
content, rather than repeatedly acquiring snapshots or retaining unrelated data.

## Lifecycle bug and correction

`StoreFs` retained one repository reader per open file handle, then dropped that
reader on an ordinary FUSE thread. `metadata/pins/runtime.rs::queue_release`
requires a current Tokio runtime to schedule collection-pin release. Without
one, it logs an error and leaves protection behind. As files were read and
closed, the pin inventory grew. The original standard run's first 256 small
files had approximately 3 ms median latency; the last 256 had approximately
12 ms median latency in each of its three repetitions.

`OpenBlob` now retains the owning runtime handle and enters it when dropping its
reader. This covers normal release, mount teardown, and the final reference
held by an in-flight read. It preserves collection protection until the reader
actually dies and lets the repository schedule its normal cleanup afterward.

The regression `readers_release_collection_protection_on_plain_fuse_threads`
uses real repository readers. It removes their root and runs collection while
they are live, verifies that their data survives, then closes them on a plain
thread, flushes cleanup, and verifies that collection reclaims the data. It
failed before the fix and passes for all three destruction paths afterward.

The first direct-reader probe had the same runtime-context mistake. Its standard
run (`linux-read-controls.json`) was interrupted and remains incomplete; its
smoke run (`linux-read-controls-smoke.json`) also predates the correction. Neither
is accepted performance evidence. The corrected direct controls enter the
runtime and record the wait for pending cleanup separately.

## Permanent controls

The `filesystem-transports` manifest entry runs these in both smoke and standard
profiles and through `benchmark all`:

- Cached pathlib reads, POSIX open/pread/close, open/close alone, and in-memory
  SHA-256, using the same small-file and size-boundary fixture.
- The same 256-byte file read 2,055 times (39 in smoke), either reopening it or
  keeping its descriptor open. Both validate identical bytes with SHA-256.
  The held-descriptor case excludes its one setup open/close, and is explicitly
  not equivalent to an application opening 2,055 different paths.
- Direct Rust repository reads over the same 2,055 files, reporting open, read,
  and reader-drop time separately, with full byte correctness checks.
- The same direct case using one existing `RetainedReader` session. Session
  admission and cleanup are recorded separately. The two API variants alternate
  order between repetitions and each performs two passes.

Cached controls run after the original first/repeat cases; the direct controls
run after the cached controls. This is warmed backing storage, not cold disk.
Direct Rust controls use byte equality, while the Python client uses SHA-256;
their wall times are for attribution, not an exact transport-overhead subtraction.
The shared read session retains an entire snapshot and can delay collection of
unrelated data. It is a benchmark control, not a production mount change.

The runnable suite retains both first/repeat passes, both small/large file-count
profiles, and sizes immediately below/at/above 4 KiB and 128 KiB. The discovered
growth with successive opens is not a newly identified fixed byte threshold.

## Reproduce and validation

```sh
cargo build -p casita-fs --release --all-features --example transport_server
python3 -m benchmarks.cli run filesystem-transports --profile standard --repetitions 3 \
  --server-binary target/release/examples/transport_server \
  --output benchmarks/results/read-controls-standard.json
python3 -m benchmarks.cli all --suites filesystem-transports --profile smoke \
  --output benchmarks/results/read-controls-all
cargo test -p casita-fs --all-features --lib
cargo clippy -p casita-fs --all-features --lib --example transport_server --no-deps -- -D warnings
python3 -m unittest benchmarks.tests.test_filesystem_transports benchmarks.tests.test_all benchmarks.tests.test_cli
```

The same commands work inside `devenv shell`. The local run used Python 3.14.7
and records compiler versions, source hashes, executable hash, host load, CPU,
and FUSE counters in JSON. The Rust tests passed (24 active, one ignored), as
did 31 Python tests and package Clippy. `read-controls-all/execution.json`
records successful execution through the permanent all runner.

Unrelated compilation was active on the shared host during the follow-up runs.
`linux-read-controls-fixed.json` records that limitation explicitly. Its timings
are diagnostic; an isolated before/after run is still required for a precise
speedup claim. Correctness and the pin-lifetime regression do not rely on timing.
