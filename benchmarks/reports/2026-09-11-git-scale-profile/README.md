# Git import staging-pin profile

The investigation keeps Git import defaults at 16 active objects and 64 MiB of
decoded staging bodies. It uses the permanent `git-scale` generators and disables
source pack caching. The production baseline is `949f145`; both comparison
binaries include the new test-only profiling and correctness harnesses.
Measurements were collected before rebasing onto later `main` commits, including
the separate blob verification-buffer change. The recorded binary hashes and
base revision identify those measurements; rerunning the comparison on current
`main` also includes those intervening changes in both binaries.

## Finding and change

A standard delta-heavy source contains 14,060 reachable objects and
2,118,555,482 logical bytes, stored in a 1,271,070-byte Git pack. A 10-second CPU
sample of its initial import attributed 69.32% of sampled cycles, including
descendants, to the local pin journal's `Index::update`. The ledger group editor
covered 98.82%. These are sampled CPU shares during that interval, not shares of
whole-import wall time. See [the CPU report](baseline-cpu.txt).

Each `Protect` operation cloned its growing staging pin, removed every old
resource from the derived index, recalculated its encoded size, and reinserted
every resource. The change retains only newly added resources for this operation
and updates their index counts and encoded-size contribution. Other transitions
retain their existing handling. Journal frames, checkpoints, replay format,
resource arbitration and acknowledgement barriers are unchanged.

The [candidate CPU sample](candidate-cpu.txt), also taken after the second
4,096-object checkpoint, no longer shows per-addition `Index::update` as a
hotspot. Journal flushes cover 94.33% of sampled cycles, including checkpointing
at 60.79% and checkpoint index reconstruction at 36.59%. These nested shares
overlap. Rebuilding the checkpoint index and serializing whole growing pins
remain follow-up targets; this change preserves the journal's byte volume.

The exploratory standard run was intentionally stopped after the CPU sample
identified this hotspot. [Its record](exploratory-standard.json) is incomplete
and supplies no successful import timing or correctness claim. Comparisons use
completed, audited runs below; the CPU profile is not a completed benchmark.

## Completed comparisons

The RAM-backed filesystem control removes disk flush latency while retaining
the same storage code and calls to durability barriers. It measures CPU and
in-memory filesystem costs; these numbers are not durable-disk latency claims.
Medians of three runs, each containing 16 additions:

| Existing resources | Before, ms/addition | After, ms/addition |
|---:|---:|---:|
| 1 | 0.426 | 0.090 |
| 8,192 | 14.435 | 4.726 |
| 16,384 | 49.784 | 15.226 |

The larger cases improve by 3.05× and 3.27×. Sub-millisecond results for the tiny
pin are noisy. Every durability metric matches exactly between binaries,
including journal bytes, checkpoints and syncs. See
[reference](pin-growth-tmpfs-before.json) and
[candidate](pin-growth-tmpfs-after.json).

The corresponding Btrfs runs are retained in
[reference](pin-growth-before.json) and [candidate](pin-growth-after.json).
They show substantial I/O variation and do not establish a disk-latency speedup.

All 54 completed smoke imports across both binaries passed their independent
inventory/ref and complete closure gates. All 27 matched comparisons preserved
exact view roots and object counts. Btrfs wall-time medians in seconds:

| Shape | Operation | Before | After |
|---|---|---:|---:|
| Many objects | Initial | 1.771 | 2.810 |
| Many objects | Unchanged | 0.035 | 0.063 |
| Many objects | Incremental | 0.380 | 0.783 |
| Delta-heavy | Initial | 3.687 | 2.379 |
| Delta-heavy | Unchanged | 0.040 | 0.029 |
| Delta-heavy | Incremental | 0.419 | 0.391 |
| Wide tree | Initial | 8.183 | 6.472 |
| Wide tree | Unchanged | 0.050 | 0.053 |
| Wide tree | Incremental | 1.019 | 0.733 |

These mixed results, including slower many-object runs and variation on the
unchanged fast path, do not support a general end-to-end speedup claim. The
phase counters and individual samples remain available in
[reference](import-before.json) and [candidate](import-after.json).

Repeating all smoke cases on a RAM-backed destination separates CPU/filesystem
work from disk waits. All 54 additional imports passed, and all corresponding
roots and object counts matched. Initial-import medians in seconds:

| Shape | Before | After |
|---|---:|---:|
| Many objects | 0.207 | 0.058 |
| Delta-heavy | 0.126 | 0.064 |
| Wide tree | 3.619 | 0.232 |

The CPU/filesystem control shows no initial-import regression in these shapes.
It is not a prediction of durable-disk performance. The individual initial,
unchanged and incremental samples, including millisecond-scale variation, are
retained in [reference](import-tmpfs-before.json) and
[candidate](import-tmpfs-after.json).

The [standard delta-heavy candidate run](import-standard-after.json) completed
and passed all three independent audits:

| Operation | Objects | Import seconds | Header + decode seconds | Peak RSS, MiB |
|---|---:|---:|---:|---:|
| Initial | 14,060 | 310.786 | 1.150 | 1,093.1 |
| Unchanged | 14,060 | 0.747 | 0 | 55.6 |
| Incremental | 14,200 | 3.586 | 0.019 | 171.3 |

Source header lookup and decoding account for less than 0.4% of initial-import
wall time here. Staging waits account for 303.210 seconds. This supports keeping
decoding on the current path while addressing storage bookkeeping. The decoded
body budget remains 64 MiB; it does not bound total RSS. This is one validation
run, includes a 10-second CPU recording, and has no completed standard reference
run from which to claim an end-to-end speedup.

## Reproduction

The permanent entry points are registered in `benchmarks/manifest.json`,
`benchmark all`, and revision builds:

```sh
benchmark run pin-growth --profile standard --repetitions 3 \
  --probe-binary /path/to/probe --no-build --output /tmp/pin-growth.json
benchmark run git-import-profile --profile smoke --repetitions 3 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /path/to/probe --no-build --output /tmp/import.json
benchmark run git-import-profile --profile standard --shapes delta-heavy \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /path/to/probe --no-build --output /tmp/import-large.json
# CPU/filesystem control; this does not measure durable-disk flush latency:
TMPDIR=/dev/shm benchmark run pin-growth --profile standard --repetitions 3 \
  --probe-binary /path/to/probe --no-build --output /tmp/pin-growth-tmpfs.json
TMPDIR=/dev/shm benchmark run git-import-profile --profile smoke --repetitions 3 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /path/to/probe --no-build --output /tmp/import-tmpfs.json
```

Build the probe with `cargo test --release --features cli,git --lib --no-run`
and copy the emitted libtest executable before building another revision.
To reproduce the reference binary in a disposable checkout of this change,
restore only `src/metadata/pins/persistent/journal/incremental.rs` from `949f145`
before building, retaining the new benchmark harnesses. Restore the candidate
file before its build. Binary SHA-256 values are recorded with every run.

CPU sampling can be attached to the libtest child of the standard command:

```sh
perf record -F 99 -g --call-graph dwarf,8192 -p CHILD_PID \
  -o /tmp/import.perf.data -- sleep 10
perf report -i /tmp/import.perf.data --stdio --children --call-graph none \
  --percent-limit 0.5 --sort symbol
```

## Method and correctness

The host is a shared Ryzen 7 7840S Linux machine on Btrfs, with filesystem usage
above the local collection threshold of 80%. Builds and source generation are
outside measured runs. OS caches are warm; sources are reused between binaries.
Shared-host I/O variation is visible in individual samples, so retain the raw
samples and treat small wall-time differences cautiously.

Import timing excludes repository opening and the independent post-import
audit. Header/decode, staging waits, checkpoint publication, root publication,
compaction and mutation admission are recorded separately. Verification and
upload sums overlap the staging waits and must not be added to them. Linux
high-water RSS is sampled immediately after import, before loading the expected
inventory and auditing the closure; it includes repository opening.

Each successful import must match an independent Git inventory and main ref,
have the same persisted view identity, and pass complete closure verification.
Unchanged imports must preserve the initial root. Sources pass strict Git fsck.

The pin-growth benchmark seeds a staging pin outside timing, then measures 16
sequential durable additions. Fixed-width resource paths place 8,192 and 16,384
resources below and above the 1 MiB journal window. After timing it discards the
cache, replays the exact resources from disk, rejects deletion of protected
resources, releases the pin, and confirms deletion can proceed without leaks.
Both sides of the window are retained in smoke and standard profiles.

Validation: 68 pin tests and six Git repository tests passed; the Python
benchmark/dashboard/comparison checks ran 51 tests with one existing skip.
Clippy passed with all features and targets and warnings denied. Rust formatting
and whitespace checks passed. The completed corpus contains 111 audited import
samples and 36 pin-growth samples; the intentionally stopped exploration is
excluded from those counts.
