# Linux reads without sudo

Continue with host FUSE through the installed `fusermount3` helper. Kernel EROFS
remains excluded by the requirement to mount without sudo. No macOS measurements
were made in this investigation.

The implemented change reuses up to eight idle query-only Turso connections.
Each live snapshot owns an exclusive connection and a fresh read transaction.
Release attempts rollback immediately; only a successful rollback with autocommit
restored returns the connection to the pool. A pending or failed rollback discards
the connection. Cleanup requires no ambient async runtime and runs no nested
executor. Live readers never wait for a cache slot; connections above the idle
bound are discarded on release.

This preserves per-object GC protection and FUSE open/release behavior. It does
not retain a whole repository snapshot to accelerate unrelated file reads.
Zero-message FUSE opens remain deferred because they also remove release
notifications needed for the current reader lifetime model.

## Measurements

[Snapshot standard results](snapshot-standard.json): 30 passing cases, three
repetitions, 256 measured bursts per case after one warm-up burst. A writer commits
a new generation outside each timed burst; readers must see exactly that
generation. Timing includes acquisition, a checked query, and release, divided by
the number of snapshots. The fresh control clears the candidate's idle pool after
each burst, including destruction in timing. It is not a historical implementation
comparison. Both modes use the same release binary.

| Simultaneously live snapshots | Fresh median, µs/snapshot | Reused median, µs/snapshot |
|---:|---:|---:|
| 1 | 767.73 | 215.31 |
| 7 | 66.29 | 52.64 |
| 8 | 59.95 | 32.04 |
| 9 | 78.17 | 34.38 |
| 16 | 59.49 | 31.07 |

The component results favor reuse, including above the idle bound. This is a
shared development host with unrelated workloads and substantial variation:
single-reader reused samples ranged from 158 to 670 µs. These results do not
establish a guaranteed speedup or a latency floor.

[Mounted standard results](mounted-standard.json): three repetitions, 216
samples, and twelve direct-reader passes. These used native Linux host mounts
without sudo; the installed mount helper was available. For the 2,055 small and
boundary files:

| Repetition | First mounted read pass, seconds | Cached mounted pass, ms |
|---:|---:|---:|
| 0 | 6.180 | 70.0 |
| 1 | 7.434 | 96.3 |
| 2 | 31.466 | 798.5 |

First-pass backing storage was warmed by import; this is not a cold-disk test.
During the last first-pass sample the one-minute host load rose from 29.1 to 36.4.
No builds or tests from this investigation overlapped these standard measurements,
but unrelated builds were observed on the host. Keep the slow repetition in the
results. The earlier [pre-pool diagnostic](../2026-09-10-filesystem-transports/linux-read-controls-fixed.json)
had an 8.16-second first-pass median; these separate runs do **not** establish a
causal end-to-end improvement. Cached reads still pay FUSE open round trips, so
this change does not establish a reduction in the previously discussed ~70 ms.

## Correctness and security checks

- Snapshot tests verify old-reader isolation, fresh commits from a second database
  handle after reuse, write rejection, recovery after a failed query, cleanup on
  plain threads and within a futures executor, and the idle bound at 7/8/9/16.
- The repository run passed 630 tests initially. All 34 failures were inability to
  start `rustfs` from PATH; rerunning the remote-storage module with its installed
  binary available passed all 59 tests, including those 34.
- All 24 filesystem tests passed with `/dev/fuse` and `fusermount3` available.
- Mounted benchmarks checked exact file bytes, boundary sizes, directory names,
  mmap, executable behavior, denial of seven mutation operations, sandbox path
  denial with a readable source control, independent mounts, and teardown.
  These are scoped checks, not a general IPC authentication or security proof.
- Core and filesystem Clippy checks passed with `--no-deps -D warnings`; the
  benchmark runner tests passed, and formatting and `git diff --check` passed.
- The new suite is registered in the manifest, revision runner, dashboard, and
  `benchmark all`. Its [all-suite smoke ledger](snapshot-all/execution.json)
  records ten passing cases. Smoke ran during validation work and is not used
  for performance conclusions. Binary hashes are retained; generated executables
  were moved outside this report directory.

## Reproduce

```sh
benchmark run snapshot-connections --profile standard --repetitions 3 \
  --output benchmarks/results/snapshot-connections.json
benchmark all --suites snapshot-connections --profile smoke --repetitions 1 \
  --output benchmarks/results/snapshot-connections-all
cargo build -p casita-fs --release --all-features --example transport_server
benchmark run filesystem-transports --server-binary target/release/examples/transport_server \
  --profile standard --repetitions 3 --output benchmarks/results/rootless-mounted.json
```

On this NixOS host the commands used
`/nix/store/3n4qphl9s728sz8frmpqqrv9b1m87g68-python3-3.14.7/bin/python3 -m benchmarks.cli`
instead of the `benchmark` wrapper, and `/run/wrappers/bin` was added to PATH for
mounting. The remote test rerun added
`/nix/store/4mfq60fwgxiz76af76d9ifqcvyi0f73k-rustfs-1.0.0-rc.1/bin` to PATH.

Before drawing an end-to-end performance conclusion, run paired before/after
release binaries on a quiet Linux host with the same corpus and correctness
gates. The remaining large first-read cost is repository admission; replacing
its protection with broad snapshot retention would change GC behavior.
