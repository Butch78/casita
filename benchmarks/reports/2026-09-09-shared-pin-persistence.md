# Shared local pin persistence — 2026-09-09

The shared implementation, both platforms’ shared correctness and permanent
benchmark sweeps, and the paired Obrador comparison are complete. macOS improves in both measured graphs;
Linux does not show a reliable improvement.

Casita `4e7b5be711e3f6b8780d5aedd60b6af99498e141` enables the same local journal,
replay cache, incremental indexes, queued groups, deferred writes, and checkpoints
on Linux and macOS. The baseline is main
`59c2fbdcb1f10b0df607e5976c797f39e776ce3f`, rebased before implementation.

The changes since the original diagnostic's `b7be320` include the separate
process-owned retained-reader change (`6a42e09`) and benchmark updates. Both
sides of this comparison include that change. Its implementation is untouched.
The supplied b7be320 traced timings are historical diagnostic evidence, not the
before samples for this comparison.

## Implementation and safety

The checkpoint filesystem wrapper uses Rustix's atomic exchange operation:
Linux `RENAME_EXCHANGE`, macOS `RENAME_SWAP`. Unsupported exchange returns an
explicit error. It never substitutes multiple renames or silently downgrades an
active journal. Portable size accounting, reader admission, statistics, and the
journal correctness/benchmark modules now also compile on macOS.

The three full-inventory GC operations—collector acquisition, validating prune,
and validated deletion claims—were already handled by the rebased baseline.
They retain merged-reader validation, then journal their sparse differences.
A new mixed-operation test verifies these paths, exact cold replay, retired-pin
cleanup, protected-claim rejection, and one durability barrier per group.

Additional tests cover rejected exchange before migration and on an active
journal, and kernel ENOSPC on a disposable volume. The existing suite covers
ordered conditional operations, cancellation, failed writes, torn/corrupt frames,
checkpoint crash points, independent processes, readers, and capacity-denied GC.

Rust 1.96's `File::sync_all` uses `fsync` on Linux and `F_FULLFSYNC` on macOS.
The existing synchronization calls and error propagation remain in place. File
capacity is grown before acknowledgment, but allocated length is not a universal
promise that future filesystem metadata or copy-on-write updates need no space.

## Correctness evidence

| Gate | Linux | macOS |
|---|---|---|
| Shared pin unit/protocol suite | 64 passed | 64 passed |
| Dedicated full-volume gate | Passed, tmpfs | Passed, APFS disk image |
| Reserved checkpoint at ENOSPC | Succeeded | Returned `StorageFull` safely |
| Append after that checkpoint attempt | Succeeded | Succeeded |
| All acknowledged pins recovered after freeing space | Passed | Passed |
| All-features suite | 773 passing test executions, no failures | Not run |
| Retained-process/publication integration tests | Included in all-features | 6 passed |
| Clippy, all features/targets, warnings denied | Passed | Not run |
| Documentation validation and links | Passed | Not run |

The five ignored entries in each pin-suite invocation are benchmark/child-process
helpers and the opt-in full-volume test; the full-volume test was run separately.
The macOS integration invocation also named `online_pin_ledger`, but that entire
binary is gated on `s3` and ran zero tests. It is not credited as macOS coverage.
Local journal process/recovery coverage comes from the shared pin suite.

Raw [Linux logs](2026-09-09-shared-pin-persistence/linux/pins-tests.log),
[all-features log](2026-09-09-shared-pin-persistence/linux/all-features-tests.log),
[macOS logs](2026-09-09-shared-pin-persistence/macos/pins-tests.log), and
[macOS integration log](2026-09-09-shared-pin-persistence/macos/integration-tests.log)
retain individual test names. The
[Linux](2026-09-09-shared-pin-persistence/linux/full-volume.log) and
[macOS](2026-09-09-shared-pin-persistence/macos/full-volume.log) full-volume logs
retain the differing checkpoint outcomes.

## Permanent benchmark coverage

`durable-ledger` and `ledger-boundaries` remain registered in
`benchmarks/manifest.json` and included in `benchmark all`. The batch sweep now
also covers 1/2 operations. Every profile includes 1/2 and 63/64/65 group sizes,
255/256/257 journal operations, the third/fourth/fifth 256 KiB catalog record at
the 1 MiB frame-window boundary, and denied/allowed migration growth.

Counters retain pin operations, groups, maximum group size, journal frames and
bytes, append/checkpoint syncs, checkpoints, adoptions, replacements, cached edits,
inventory copies, and inventory diffs. Capacity-growth and replay-adoption syncs
are not included in `journal_syncs`; it is not a count of every filesystem sync.

The replacement control uses preallocated inventory slots in the same binary.
On macOS it is **not** the historical temporary-file implementation. The actual
before/after comparison is Obrador with separately pinned Casita revisions.

Both platforms passed 39 boundary and 648 aggregated ledger samples each, using
identical configurations and three repetitions. Every sample has a correctness
gate; setup and cold-replay audits are outside timing. The
[Linux boundary](2026-09-09-shared-pin-persistence/linux/boundaries.json),
[Mac boundary](2026-09-09-shared-pin-persistence/macos/boundaries.json),
[Linux ledger](2026-09-09-shared-pin-persistence/linux/ledger.json), and
[Mac ledger](2026-09-09-shared-pin-persistence/macos/ledger.json) files retain all
samples and captured probe output.
Raw results include binary hashes, process output, environment identity, and
counter deltas. The [source record](2026-09-09-shared-pin-persistence/source.json)
identifies the implementation and frozen Obrador source.

## Linux Chain rerun

A fresh three-round Chain-64 rerun reused the exact same retained baseline and
after plugin binaries (hashes checked against the original comparison), eight
jobs, alternating variant order, fresh private stores, and no diagnostic trace
filter. All 12 Nix/Obrador trials passed: 768 build completions and 12 requested
output contents verified. No invocation failed or was excluded.

| Round | Baseline seconds | After seconds |
|---|---:|---:|
| 1 | 64.738 | 80.765 |
| 2 | 85.353 | 40.876 |
| 3 | 42.834 | 63.343 |
| **Median** | **64.738** | **63.343** |

The after median is 2.2% lower. This rerun does
not reproduce the earlier 60% median slowdown, but the large variation in both
versions prevents a reliable speedup or parity claim from these samples alone.
These results are kept separate from the earlier run; they are not pooled or used
to erase its preparation timeout. Nix remains the per-trial comparator.

[Raw samples and summary](2026-09-09-shared-pin-persistence/linux/chain-rerun/summary.json),
[exact commands and plugin hashes](2026-09-09-shared-pin-persistence/linux/chain-rerun/execution.json),
and [retained verification logs and hashes](2026-09-09-shared-pin-persistence/linux/chain-rerun/files.json)
are saved alongside the other evidence. This repeats the existing permanent
Chain workload; the Casita ledger threshold corpus remains registered unchanged.

## Obrador results

macOS matched untraced medians (three samples per version/workload):

| Workload | Baseline seconds | After seconds | Change |
|---|---:|---:|---:|
| Chain-64 | 71.855 | 41.192 | -42.7% |
| Wide-64 | 47.256 | 20.727 | -56.1% |

Chain baseline samples span 70.9–73.9 seconds, after samples 41.0–42.4 seconds.
Wide baseline samples span 45.9–47.6 seconds, after samples 20.7–22.0 seconds.
All six paired invocations and both separate diagnostic invocations succeeded;
no samples were excluded. All 28 Nix/Obrador trials passed their completion and
content gates: 1,792 builds and 462 requested outputs. Nix comparator times were
1.59–1.64 seconds for Chain and 1.29–1.34 seconds for Wide (rounded).
The improvement does not close the remaining gap to Nix.

The [selection and exact samples](2026-09-09-shared-pin-persistence/macos/obrador/selection.json),
[commands and plugin hashes](2026-09-09-shared-pin-persistence/macos/obrador/execution.json),
and [retained-file hashes](2026-09-09-shared-pin-persistence/macos/obrador/files.json)
make the comparison reproducible. All retained file hashes were checked locally;
all four compressed diagnostic traces reproduce their saved summaries exactly.

macOS diagnostic phase counts:

| Workload/version | Inventory replacements | Journal append syncs | Checkpoints |
|---|---:|---:|---:|
| Chain baseline | 4,192 | 0 | 0 |
| Chain after | 0 | 3,942 | 215 |
| Wide baseline | 2,589 | 0 | 0 |
| Wide after | 0 | 1,787 | 241 |

The historical inventory-replacement path synced each replacement file and its
parent directory. The shared path appends a group with one journal sync and uses
file/directory syncs for checkpoints. The phase counts confirm elimination of
per-operation inventory replacement and fewer barriers along these persistence
paths; they are not an exhaustive syscall census. Allocation, reader files, and
adoption can require additional syncs. Inclusive durations nest and are not added.
Raw traces and diagnostics are under `macos/obrador/trace-baseline` and `trace-after`.


Linux matched untraced medians (three samples per version/workload):

| Workload | Baseline seconds | After seconds | Change |
|---|---:|---:|---:|
| Chain-64 | 15.852 | 25.378 | +60.1% |
| Wide-64 | 19.172 | 17.759 | -7.4% |

Chain baseline samples span 15.4–31.9 seconds, after samples 22.8–28.3 seconds.
Wide baseline samples span 11.3–19.6 seconds, after samples 14.5–18.9 seconds.
The observed Chain median regression is retained explicitly; these variable
shared-host data do not establish Linux performance parity or improvement.
The third matched pair ran after a reboot. See the exact
[selection](2026-09-09-shared-pin-persistence/linux/obrador/selection.json),
[execution record](2026-09-09-shared-pin-persistence/linux/obrador/execution.json),
and [retained-file hashes](2026-09-09-shared-pin-persistence/linux/obrador/files.json).
The 24 selected untraced trials and four separate traced trials each passed their
build-completion and output-content gates (1,792 completions, 462 outputs).

Linux diagnostic counts (durations are not added or used as untraced timings):

| Workload/version | Inventory replacements | Journal append syncs | Checkpoints |
|---|---:|---:|---:|
| Chain baseline | 0 | 3,929 | 215 |
| Chain after | 0 | 3,929 | 215 |
| Wide baseline | 0 | 1,687 | 338 |
| Wide after | 0 | 1,848 | 247 |

Linux already used the journal. Chain's counts are identical; concurrent Wide
batching varies with scheduling. These are phase-event counts, including any attempted phase. Each successful
checkpoint also has file and directory syncs. These counts do not include every allocation, adoption, or reader-file sync.
Compressed raw traces, per-phase diagnostics, full commands, and verification
output are retained under `linux/obrador/trace-baseline` and `trace-after`.
The collector validates every successful trial and records hashes before and
after compression; `analyze-trace.py TRACE.gz --diagnostics` reproduces summaries.

## Reproduction

From Casita's pinned devenv on each platform:

```sh
cargo test --lib metadata::pins -- --nocapture
cargo test --features experimental --test retained_process_pins --test publication_snapshot
cargo test --all-features
cargo clippy --all-features --all-targets -- -D warnings
benchmark run ledger-boundaries --repetitions 3 --output results/boundaries.json
benchmark run durable-ledger --counts 64,4096 --writers 1,8,64 --iterations 2 \
  --repetitions 3 --contexts quiet,readers,claims --output results/ledger.json
benchmark all --suites durable-ledger,ledger-boundaries --profile smoke \
  --repetitions 1 --output results/all-ledgers
```

The measured microbenchmark invocations use `--no-build --probe-binary PATH`
with the retained optimized `cargo test --release --features cli --lib` binary.
The harness requires GNU time on `PATH`, including on macOS; the system BSD time
is insufficient. The [Mac invocation record](2026-09-09-shared-pin-persistence/macos/microbenchmark-execution.json)
retains exact commands and setup corrections. Linux probes were built before the
implementation commit, so their environment records show baseline HEAD plus a
dirty worktree; retained binary hashes and `source.json` identify the tested build.
Both platforms use Rust 1.96.0 for these gates and probes. Obrador's existing
pinned environment uses Rust 1.95.0 and ABI-matched Nix
`2.36.0pre20260901_f8cd4ce` on both platforms.

For real ENOSPC, mount a dedicated disposable filesystem smaller than 512 MiB,
then run the ignored gate. Do not point this at a general-purpose filesystem.
The test fills a temporary file, attempts a checkpoint and registration, frees
the filler, verifies acknowledged pins, and completes collector cleanup.

```sh
CASITA_LEDGER_FULL_DIR=/path/to/disposable-volume cargo test --lib \
  metadata::pins::persistent::journal::tests::physically_full_volume_preserves_acknowledged_state_and_recovers \
  -- --exact --ignored --nocapture
```

Linux used a 32 MiB tmpfs in an unprivileged user/mount namespace. macOS used
`hdiutil create -size 256m -fs APFS`, attached at a task-specific mountpoint and
detached after the test. Both are kernel-full filesystems; neither is a destructive
physical-disk exhaustion test.

Obrador uses the existing permanent `benchmarks/builds/compare.py` runner,
Chain-64 and Wide-64, eight jobs, fresh private stores, sandboxing, no substitutes,
and no remote builders. Three untraced rounds alternate baseline/after order.
Separate diagnostic runs enable `obrador=debug,obrador_core=debug,casita=debug`.
Build time is excluded. Runs without the diagnostic filter still record the
build-completion events needed for correctness. Every successful trial verifies all 64 completions and
the requested output bytes (one Chain leaf, 32 Wide leaves).

```sh
python3 benchmarks/builds/compare.py --nix /path/to/abi-matched/nix \
  --plugin /path/to/pinned/libobrador --workloads chain wide --count 64 \
  --jobs 8 --rounds 1 --backends nix obrador --timeout 900 --output fresh-output
# Separate diagnostics, never pooled with untraced timing samples:
python3 benchmarks/builds/compare.py --nix /path/to/abi-matched/nix \
  --plugin /path/to/pinned/libobrador --workloads chain wide --count 64 \
  --jobs 8 --rounds 1 --backends obrador --timeout 900 \
  --trace obrador=debug,obrador_core=debug,casita=debug --output fresh-trace-output
```

Linux additionally passes the pinned static BusyBox through `--shell`. Full
commands, plugin hashes, timing boundaries, and source manifests are retained
with the results. Inclusive trace durations nest and must not be added together.

## Remaining limits

APFS returned `StorageFull` for a checkpoint despite both slots already having
sufficient length. The operation failed safely, and acknowledged state survived;
reserved length does not guarantee GC progress through arbitrary APFS exhaustion.
Unsupported exchange is fault-injected; no claim is made about every filesystem.
Process-kill recovery is tested, not physical power removal or every storage
controller's persistence behavior. Full-inventory GC validation remains
conservative and can still copy/compare the inventory.

The initial Linux third-after Obrador trial timed out after 900 seconds while
copying inputs, before the timed build. Its partial results and failure log are
retained, and it is excluded from successful timing samples. A fresh matched pair
completed successfully, but the timeout’s cause has not been established.
Shared-host timings carry variability; no performance target is assumed.
