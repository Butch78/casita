# Filesystem transport baseline: performance and security

The permanent `filesystem-transports` suite serves the same deterministic fixture
from the host filesystem and a real Casita mount. Linux uses the current
`fuse-backend-rs` transport; macOS uses the current patched `fuser` → FUSE-T → FSKit
path. This is baseline infrastructure, not evidence to select a replacement.

Follow-up: [read-cost investigation and Linux reader-lifetime fix](read-costs.md).
The original first-read figures below predate that fix and include accumulating
collection pins; they should not be attributed solely to transport overhead.

The configured Mac (`hetzner@23.88.76.133`) timed out on SSH on 2026-09-10. Native
FSKit measurements, its runtime authentication audit, and candidate comparisons
remain outstanding. Linux measurements cannot answer the macOS transport choice.

Source inspection identifies a concurrency constraint to test next: vendored
`fuser::Session` refuses `n_threads != 1` outside Linux, and
`darwin/frontend.rs` keeps its state mutex locked across `FilesystemView::read`.
These are hypotheses about the macOS bottleneck, not measured scaling results.
Moving to a different transport alone would not remove the frontend lock.

## Measured Linux baseline

`linux-standard-final.json` contains 108 accepted samples from three repetitions
on an AMD Ryzen 7 7840S, Linux 7.2.2, btrfs, and the performance CPU governor.
No other benchmark or build from this investigation overlapped this final run.
The earlier `linux-smoke.json` and `linux-standard.json` are harness-validation
artifacts only: they overlapped other work and are not comparison baselines.

These are medians of the three workload wall times, in milliseconds. Each cell
shows first pass / repeat pass; throughput and individual latencies are in JSON.

| Workload | Host | Casita Linux FUSE |
| --- | ---: | ---: |
| Stat 2,057 files | 19.89 / 19.63 | 63.58 / 19.60 |
| Read 2,055 small/boundary files | 30.66 / 30.36 | 16,357.59 / 69.45 |
| Read 64 MiB three times | 192.51 / 190.94 | 273.82 / 191.30 |
| mmap/hash 64 MiB three times | 90.99 / 89.87 | 110.19 / 111.16 |

The largest observed cost is the first small-file read pass. Every repetition
opened 2,055 repository blobs and issued 2,056 FUSE reads, returning 929,807 bytes.
Median server CPU consumption during that case was 16.95 seconds. Repeat passes
issued 2,055 FUSE opens but no FUSE reads or repository blob opens: the kernel
served cached content. This identifies a costly uncached read path, but does not
separate repository work from transport overhead. The permanent suite retains
both passes and the sizes on both sides of the planned boundaries.

All listed Linux mutation-denial, sandbox-path, independent-mount, integrity, and
teardown gates passed. This is not an IPC authentication or full security audit.
The Python harness checks passed (30 tests), the Rust example built in release
mode, and package Clippy passed with `--no-deps -- -D warnings`. Dependency-wide
Clippy encounters existing vendored `fuser` lint errors. `all-execution.json`
retains the successful `benchmark all --suites filesystem-transports` integration
run; its timings are for harness validation only.

The next performance investigation should isolate repository open/read costs
with a direct-reader control before changing transports. On an accessible Mac,
measure the current FSKit path, profile the frontend lock, and audit runtime IPC
identity/session isolation before admitting replacement candidates to comparison.
Keep correctness and security gates mandatory while ranking passing candidates
by measured application performance.

## Reproduce

```sh
devenv shell -- benchmark run filesystem-transports --profile smoke --repetitions 3 \
  --output benchmarks/results/filesystem-transports-smoke.json
devenv shell -- benchmark run filesystem-transports --profile standard --repetitions 3 \
  --output benchmarks/results/filesystem-transports-standard.json
devenv shell -- benchmark all --suites filesystem-transports --profile smoke \
  --output benchmarks/results/filesystem-transports-all
```

Without the development shell, use `python3 -m benchmarks.cli` instead of
`benchmark`, with Rust, a C toolchain, `protoc`, and the platform's mount runtime
available. `--server-binary` supplies a prebuilt release `transport_server` example.
The suite copies and hashes the executable before running. `--host-only` explicitly
collects a host control and makes no mount/security claim. Work directories,
fixtures, repositories, and server logs are retained and their locations recorded.

The macOS server uses `PersistentMount::new`, including its existing setup behavior.
An installed and enabled runtime is preferable for steady-state mounting results;
first-use setup is included in `mount_seconds` otherwise. Import time is separate.
Use an ordinary user; these probes do not need root on a configured FUSE/FSKit host.

## Cases and interpretation

- Smoke: 32 small files, plus 4,095/4,096/4,097-byte and
  131,071/131,072/131,073-byte files, an 8 MiB random file, a real executable,
  non-UTF-8 filename, and ordinary/byte-target symlinks.
- Standard: 2,048 small files, the same byte boundaries, and a 64 MiB random file.
- Stat, readdir, small/boundary reads, sequential reads, mmap, execution, and
  random open/read with 1/4 clients (plus 16 in standard). The server thread count
  is separate; the baseline defaults to one server thread.
- Both first and repeat passes are retained. They are **not cold-disk samples**:
  fixture creation and import warm backing storage, and earlier operations warm
  caches for later operations. Genuine cold-cache work remains a separate study.
- Each operation's latency and byte count is retained with its task input, so
  individual size boundaries remain identifiable inside the grouped read case.
  The boundaries are planned coverage, not a claim that a cliff was discovered.
- Client checksums/verification are inside the timed workload, identically for
  host and mount. Throughput includes those client costs. Client CPU is separate
  from server CPU; executable child CPU is not included in client CPU. Peak RSS
  is a process-lifetime high-water mark, not an operation-local memory measurement.
- Server snapshots include Casita directory fetches/blob opens; Linux additionally
  reports FUSE operation counts, bytes, and handler summaries. Load averages record
  ambient host activity. Alternate host/mount ordering across repetitions.

Do not benchmark two configurations concurrently or overlap accepted measurements
with builds. Early harness-validation runs may overlap compilation; they establish
correctness only. Use fresh output names for subsequent measured runs and retain
the recorded artifact/source identities.

## Security and correctness gates

Every accepted mounted run checks all bytes, exact directory names, symlink bytes,
executable modes, real execution, and mmap. It rejects successful writes, truncate,
chmod, unlink, rename, mkdir, and symlink creation. An isolated child must read an
allowed host control while being unable to read the mounted fixture: bubblewrap
hides the mount on Linux, and Seatbelt denies its path on macOS.

A second live mount serves a disjoint canary namespace. Neither mount may expose
the other's roots, and stopping the second must leave the first readable. Explicit
teardown must complete, the server must exit successfully, and the mount must be
absent afterward. Failures invalidate collected samples and keep the result
incomplete. Missing sandbox tools are explicitly unavailable, never passed.

On macOS, publication must become visible after a cached negative lookup and
directory listing. The harness checks the actual `fskit` mount and inspects session
descriptor/socket ownership and private parent directories without recording
descriptor contents or credentials. **This does not prove IPC authentication.**
The macOS security result remains incomplete until peer identity, session routing,
and possible sandbox bypass through IPC have been tested.

The threat boundary here is untrusted sandboxed code and other users, not arbitrary
unsandboxed processes with the same user identity. Mount-path denial alone is not
proof that all paths to repository data are denied. Adversarial framing, aborted
reads, busy-unmount failures, and cache-policy-dependent disclosure still require
dedicated gates before replacing the transport.

`complete` means the requested benchmark run finished; `security_complete` covers
only its listed gates. `decision_eligible` stays false for this baseline suite:
neither a candidate comparison nor a complete runtime security review has occurred.
