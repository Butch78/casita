# Native FSKit versus production fuser on Casita storage

## Result

Native Rust FSKit can serve a real Casita repository without root or an
application TCP bridge on this Mac. Both adapters pass the mounted correctness
and lifecycle checks. Keep this as an experimental backend and retain fuser
while completing deployment, cancellation and application-workload validation.

Host: Apple M1, 8 cores, 16 GiB, macOS 26.6.2; UID/effective UID 501. The extension
uses ad-hoc development signing and scoped repository access through
`FSPathURLResource` (macOS 26+). Our host/extension code is Rust, using objc2 and
Apple frameworks. This does not make Apple's frameworks Rust or prove signed
distribution. No root or system security changes were used.

## Prior issues revisited

Reviewed the [earlier fskit-rs assessment](https://github.com/cachix/casita/blob/c93e8280eb7e0ee91ee71935ca4290057761e7a4/casita-fs/evaluations/fskit-rs/README.md),
[bootstrap findings](../2026-09-10-fskit-bootstrap/README.md),
[setup reuse](../2026-09-12-native-setup-reuse/README.md), and
[transport investigation](../2026-09-10-filesystem-transports/README.md).

| Earlier concern | Current evidence / remaining limit |
| --- | --- |
| Rootless setup and activation | Native build, ad-hoc registration, activation and mounts passed as UID 501; activation receipt retained. |
| TCP endpoint and authentication | Native repository reads run inside the extension; no application TCP server and no TCP listener observed. FUSE-T socket permissions are recorded; same-user authentication remains unverified. Sandbox path denial is not an IPC authentication test. |
| Global endpoint / namespace collisions | Two distinct repositories mounted simultaneously on each adapter; disjoint canaries and independent teardown passed. |
| FUSE/FSKit inode numbering | Separate native inode mapping; nested traversal and publication passed. |
| Cached missing names / directory listings | Prior negative lookup and listing followed by publication and read passed on both. Native publication uses a pre-staged descriptor, not the final production API. |
| Readers outliving stop | Native backend ownership check and repository flush passed; fuser session join, publisher drop and flush passed. Cancellation during active I/O and crash recovery remain untested. |
| Shared resource / forced detach | Native mounts use separate scoped repository paths. Busy ordinary unmount leaves existing readers usable on both. Native reports resource busy; FUSE-T reports bad file descriptor, so equivalent errno behavior is not claimed. Final native unmount is ordinary; fuser uses existing production force-unmount policy. |
| Sandbox / write permissions | Denied mount/backing-store reads in sandboxed child, with readable host control. Immutable mutation gates passed after fixing production fuser callbacks to return EROFS instead of ENOSYS. |
| Executables / mmap | Script, own Rust native executable, mmap, symlink, nested reads and EOF passed on both. |
| Setup receipt reuse | Previous inode-to-content receipt fix remains applicable to existing setup. This suite excludes compilation/setup from timed operations; it does not benchmark replacement setup receipt reuse. |
| Arbitrary byte names | Earlier memory regression still shows fuser/FUSE-T rewriting invalid UTF-8. This real-repository fixture uses portable names; it does not resolve that regression. |

## Measurement design and limitations

[Final standard run](standard-rewarmed.json.gz): **complete**, all 205 pairs and
cleanup passed, no benchmark mounts/processes left. Rewarming after lifecycle
probes passed. Times below are medians of five per-round p50s; ratios are medians
of paired ratios, which need not equal division of the displayed columns.
Above 1 means lower native latency. The range is across the five paired ratios.

| Case | Native p50, ms | fuser p50, ms | fuser/native median (range) |
| --- | ---: | ---: | ---: |
| Directory listing | 16.4193 | 1741.1143 | 103.69× (9.79–638.27) |
| Stat 256 files | 2.2549 | 1.5982 | 0.89× (0.52–1.27) |
| Open/read/close 4 KiB | 0.0207 | 0.6615 | 33.42× (21.25–721.55) |
| Open/read/close 64 KiB | 0.0248 | 0.5647 | 24.71× (16.09–1197.86) |
| Open/read/close 1 MiB | 0.0823 | 0.6573 | 7.99× (5.25–76.13) |
| Held-descriptor read 4 KiB | 0.0010 | 0.0010 | 1.04× (1.00–2.74) |
| Held-descriptor read 1 MiB | 0.0584 | 0.0625 | 1.08× (0.97–3.06) |
| 32 reads / 1 worker | 3.6143 | 14.7545 | 11.18× (3.88–270.46) |
| 32 reads / 4 workers | 2.9303 | 8.3155 | 7.56× (2.84–129.97) |
| 32 reads / 16 workers | 3.3080 | 9.4932 | 5.97× (2.85–111.79) |
| Script launch | 109.5954 | 4.7870 | 0.06× (0.02–1.37) |
| Native executable launch | 59.5760 | 5.5037 | 0.10× (0.04–13.34) |

**Do not switch defaults based on read speed:** native execution is substantially
slower in most rounds. Execution is a permanent paired workload, not an omitted
exception. Investigate executable-open / code-signature / cache behavior; this
run does not identify which accounts for the difference.

Fuser's 30 directory listings took 28, 42, 306, 601 and 75 seconds across rounds;
round 2 p95 was 44.56 seconds. Native's corresponding totals stayed below 1.04
seconds. Across each entire timed round, native made 96 repository directory
calls; fuser made 3,634–8,599. This is evidence of different metadata work, not
proof that all latency comes from the transport. Blob opens after warm-up were
native `[16, 30, 0, 0, 0]`, fuser `[28, 28, 0, 0, 0]`.

Recompute the table with:

```sh
python3 benchmarks/reports/2026-09-15-native-fskit-repository/summarize.py \
  benchmarks/reports/2026-09-15-native-fskit-repository/standard-rewarmed.json
```

Both mount the same imported snapshot with `FilesystemView` and the same
instrumented `Repository::local` reader. Native uses its own metadata cache;
fuser retains the production frontend's locking/cache behavior. This compares
complete backends, not isolated transport latency.

Five alternating rounds cover 41 paired cases, including executable launch;
30 samples per regular case and three per execution case. There are 205 paired
comparisons and 448 sample rows including the separate first-touch/first-exec
observations. Boundary sizes bracket 4 KiB, 16 KiB, 64 KiB, 128 KiB and 1 MiB.
Counters record directory calls and blob opens on both sides; native-only
read/byte counters must not be compared to the fuser zeros.

The fixture import warms backing storage. First-touch samples are first reads
through fresh mounts, not cold-disk measurements, and run native then fuser
once. Warm-up follows correctness and lifecycle probes because a refused
unmount can still disturb caches. Cache residency is not guaranteed: the report
retains per-round backend counters, and some reads can reopen blobs.

Unrelated Rust builds and another filesystem workload ran throughout. Host load
and top processes are retained per round. All performance results here are
**preliminary shared-host observations**; `decision_eligible` remains false.
Python orchestration and byte validation are included in timings. Neither
memory-fixture speedups nor the best individual ratio establishes a general
Casita speedup.

A [host snapshot](host-during-slow-round.txt.gz) also recorded only about 5.9 GiB
available on the APFS data volume. The completed report predates the runner's
new configurable 1,200-second deadline and expanded cache-policy description;
source hashes retain that distinction. Neither changes the measured binaries.

During a particularly slow fuser listing, the
[Python sample](rewarmed-python-sample.txt.gz) waited in `getdirentries64` and the
[fuser sample](rewarmed-fuser-sample.txt.gz) showed its dispatcher servicing
`lookup` and waiting in the Tokio runtime. The round subsequently progressed.
These samples justify investigating metadata work; they do not isolate the
underlying wait or prove a deadlock.

## Retained attempts

- `smoke.json`: copied Apple executable was killed by launch constraints;
  the host copy failed too. Replaced with our own Rust executable and a required
  positive host execution control. This was not evidence of an FSKit defect.
- `smoke-executable.json`: first fuser script launch exceeded the initial
  15-second timeout; an isolated retry passed. Execution now has a 120-second
  limit and first-launch timings remain separate. Ordinary fuser teardown was
  unreliable; helper now explicitly matches production force-unmount policy.
- `smoke-lifecycle.json`: caught fuser chmod returning ENOSYS. Added explicit
  read-only denial callbacks in `casita-fs/src/darwin/frontend.rs`.
- `smoke-denials.json`: complete correctness and lifecycle smoke pass.
- `standard-before-rewarm.json`: complete initial standard run, retained as
  diagnostic evidence before adding warm-up after lifecycle probes.

## Reproduce and next steps

See [implementation and commands](../../../casita-fs/native-fskit/BENCHMARKS.md#repository-workloads).
The suite is permanent in the manifest and `benchmark all`, including its
correctness gates and boundary cases. On Linux it explicitly skips.

Local validation: 34 Python tests and three Rust library tests passed; Rust
formatting and diff whitespace checks passed. Native release compilation and
the mounted gates ran on the Mac.

Preferred next step: investigate native executable-launch latency, then run on a
quiet Mac using a representative Casita build/checkout workload, followed by
cancellation/crash recovery and distributable signing.
Then integrate a selectable native backend while retaining fuser as fallback.
Alternatively, keep fuser as default and use these permanent comparisons to
target its metadata/open overhead. Do not remove vendor patches while it is used.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Run `gzip -dk ./*.gz` in this directory
before running the summary or reproduction scripts. Uncompressed copies are ignored.
