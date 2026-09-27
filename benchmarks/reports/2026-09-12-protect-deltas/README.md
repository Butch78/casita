# Resource-addition journal frames

## Change and compatibility

The baseline is `fd2f363` plus the preceding cached-checkpoint index reuse
change. Its production diff is retained in
[reference-checkpoint-reuse.patch](reference-checkpoint-reuse.patch).
The reference binary was copied before this change and its SHA-256 is recorded
with every measurement. The candidate includes both optimizations; comparison
against that reference isolates the resource-addition journal change.
Measurements precede integration with `3a78302`, including the separate
mutation-catalog retention fix and pinned Turso WAL dependency. Those later
changes are not part of this measured before/after comparison.

Previously, adding one resource marked its entire pin for serialization. Once
the pin exceeded the 1 MiB journal window, each changed protection checkpointed
the full inventory. The new `CASDLT02` frame keeps the existing changed-record,
removed-token and global-state sections, followed by a bounded resource-addition
section encoded with the existing inventory codec. That section contains only
synthetic staging pins with new resources; it never replaces scope or catalog.

Grouped additions merge by token. A full replacement or release supersedes
preceding additions, while additions following a full replacement are included
in its final encoded state. Replay updates the derived resource index from the
additions, without copying or reindexing the retained pin. Duplicate resources,
unknown/retired targets, replacement fields, conflicting full records, invalid
metadata resources and unexpected globals in the addition section are rejected.

The new reader accepts both `CASDLT01` and `CASDLT02` frames in one journal.
An older binary rejects the new frame version; it cannot silently omit added
protection or safely continue while such frames remain in the journal. The
checkpoint format stays unchanged, so a completed checkpoint can still be read
by an older binary. File checksums, revision checks, acknowledgement barriers,
atomic checkpoint exchange, preallocated space and failure-injection points
are retained.

## Checkpoint policy and remaining cost

The 256-operation and 1 MiB accumulated-frame limits remain in force. They bound
replay and preserve existing reserved-space behavior. Small additions no longer
checkpoint merely because the retained pin is large. Periodic checkpoints still
serialize the whole inventory, so this does not make total growth cost strictly
linear: with fixed replay limits, snapshot cost per addition still grows with
inventory size. Changing that policy is a separate recovery/space tradeoff.

## Permanent corpus and reproduction

`pin-growth` and `ledger-boundaries` are registered in `benchmarks/manifest.json`
and included in `benchmark all`. Smoke retains 1, 8,192 and 16,384 resources,
covering both sides of the former record-size cliff. Standard now measures
1,024 additions per case, spanning several periodic checkpoints. The initial
512-addition reference run is retained separately and is not mixed into the
1,024-addition comparison.

The record-byte boundary cases retain catalogs just below, at and above 1 MiB.
Candidate correctness gates require three small edits to write three frames,
three syncs and exactly 12,696 journal bytes, with no checkpoint at all three
catalog sizes. Historical binaries retain their original exact-counter gates.
Operation and aggregate-byte checkpoint boundaries are also retained.

In disposable checkouts, build the reference from `fd2f363` plus the supplied
patch, and build the candidate from this change:

```sh
git apply /path/to/reference-checkpoint-reuse.patch # reference checkout only
cargo test --release --features cli,git --lib --no-run
cp /path/to/emitted/libtest /tmp/protect-REVISION
```

Run each command for both binaries, with separate output paths:

```sh
TMPDIR=/dev/shm benchmark run pin-growth --profile standard --iterations 1024 \
  --repetitions 3 --probe-binary /tmp/protect-REVISION --no-build \
  --output /tmp/protect-tmpfs.json
benchmark run pin-growth --profile standard --iterations 16 \
  --repetitions 3 --probe-binary /tmp/protect-REVISION --no-build \
  --output /tmp/protect-disk.json
benchmark run ledger-boundaries --profile standard --repetitions 1 \
  --probe-binary /tmp/protect-REVISION --no-build --output /tmp/boundaries.json
TMPDIR=/dev/shm benchmark run git-import-profile --profile smoke --repetitions 3 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /tmp/protect-REVISION --no-build --output /tmp/import.json
TMPDIR=/dev/shm benchmark run git-import-profile --profile standard \
  --shapes delta-heavy --repetitions 1 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /tmp/protect-REVISION --no-build --output /tmp/import-large.json
```

Builds and fixture setup are outside timing; benchmarks run sequentially on a
shared Linux Ryzen 7 7840S host. Disk results use Btrfs. RAM-backed controls use
the same code and durability calls on tmpfs, measuring CPU/filesystem costs
without disk flush latency. Host contention makes latency comparisons noisy;
exact write-volume and checkpoint counts provide independent evidence.
The disk comparison uses 16 additions per sample; the longer tmpfs comparison
includes periodic full checkpoints over 1,024 additions. These horizons are
reported separately.

Pin samples discard the cache after timing, compare exact replayed resources,
reject protected deletion, then release and delete without leaks. Import samples
check independent inventories, refs, persisted view identities and full closure
verification. Import time excludes repository opening and the closure audit.
Git defaults remain 16 active objects and a 64 MiB decoded-source-body budget;
these import probes disable source pack caching.

## Results

The long pin-growth comparison starts at each listed resource count and adds
1,024 resources. Counts are identical across all three repetitions. Journal
bytes include frames, checkpoints and next-header clearing, excluding setup
and initial slot allocation.

| Initial resources | Reference bytes | Candidate bytes | Reference checkpoints | Candidate checkpoints |
|---:|---:|---:|---:|---:|
| 1 | 43,135,718 | 4,452,833 | 39 | 3 |
| 8,192 | 689,832,448 | 6,344,954 | 512 | 3 |
| 16,384 | 1,336,691,200 | 8,237,306 | 1,024 | 3 |

At 16,384 initial resources journal write volume falls by 99.4%,
including all three periodic full checkpoints. Every candidate run contains
1,021 frames and 1,027 syncs: one sync per frame and two per checkpoint. The
durability contract is unchanged; there are fewer checkpoints to synchronize.
See [reference](pin-growth-tmpfs-before.json) and
[candidate](pin-growth-tmpfs-after.json).

RAM-backed median milliseconds per addition, three repetitions:

| Initial resources | Reference | Candidate |
|---:|---:|---:|
| 1 | 0.115 | 0.049 |
| 8,192 | 0.963 | 0.051 |
| 16,384 | 2.261 | 0.054 |

The disk comparison measures 16 additions per sample. All candidate sizes
write 67,712 bytes in 16 frames with 16 syncs and no checkpoint. The reference
writes 10,166,600 bytes with eight checkpoints at 8,192 resources and
20,264,872 bytes with sixteen checkpoints at 16,384 resources. Btrfs median
milliseconds per addition:

| Initial resources | Reference | Candidate |
|---:|---:|---:|
| 1 | 15.916 | 8.773 |
| 8,192 | 41.308 | 10.735 |
| 16,384 | 65.777 | 10.904 |

These disk batches improved, but host conditions contribute: even the tiny
case, whose write and checkpoint counts are identical, became faster. Keep the
exact volume reduction separate from a general latency claim. See
[reference](pin-growth-disk-before.json) and
[candidate](pin-growth-disk-after.json).

All 16 boundary cases passed for each binary. The three catalog-size cases
now write exactly 12,696 bytes, three frames and three syncs each; none
checkpoints. Operation, accumulated-byte, group-size and migration boundaries
retain their original gates. See [reference](boundaries-before.json) and
[candidate](boundaries-after.json).

The standard delta-heavy import contains 14,060 initial objects and
2,118,555,482 logical bytes. Both binaries passed initial, unchanged and
incremental audits with matching roots and object counts. One tmpfs run each:

| Operation | Reference seconds | Candidate seconds | Reference RSS, MiB | Candidate RSS, MiB |
|---|---:|---:|---:|---:|
| Initial | 64.570 | 8.476 | 707.2 | 424.0 |
| Unchanged | 0.020 | 0.026 | 59.8 | 51.1 |
| Incremental | 0.332 | 0.229 | 153.5 | 127.4 |

Initial wall time fell by 87% in this single comparison. Source header/decode
time was similar (0.997 vs 0.971 seconds), while staging poll/drain time fell
from 62.235 to 6.804 seconds. RSS is sampled immediately after import, before
the closure audit, and includes repository opening. These are RAM-backed
shared-host measurements, not a durable-disk throughput guarantee. See
[reference](import-standard-tmpfs-before.json) and
[candidate](import-standard-tmpfs-after.json).

Smoke initial-import medians, three repetitions per batch, in seconds:

| Shape | Reference | Candidate | Repeated reference | Repeated candidate |
|---|---:|---:|---:|---:|
| Many objects | 0.045 | 0.074 | 0.049 | 0.038 |
| Delta-heavy | 0.053 | 0.072 | 0.054 | 0.051 |
| Wide tree | 0.195 | 0.129 | 0.182 | 0.117 |

The first batch was mixed, so both binaries were repeated back-to-back with
the same commands and new output names. The small-case slowdowns did not
reproduce in that second pair; all samples are retained rather than selecting
the favorable batch. These short timings remain sensitive to host conditions.
Every import audit passed and corresponding roots/object counts match. See
[reference](import-tmpfs-before.json), [candidate](import-tmpfs-after.json),
[repeated reference](import-tmpfs-before-repeat.json) and
[repeated candidate](import-tmpfs-after-repeat.json).

## Validation

The release build passed 73 pin tests and six Git repository tests. The Python
benchmark/dashboard/comparison checks passed 70 tests with one existing skip.
Clippy passed with all features and targets and warnings denied. Formatting
and whitespace checks passed. The supplied reference patch was applied in a
disposable checkout and verified against the exact saved baseline source.
All 13 release benchmark reports are complete and normalize for the dashboard:
114 audited imports, 45 pin-growth samples and 32 boundary samples, 191 total.
Every candidate growth sample has exactly one frame or checkpoint per changed
protection and the required one or two durability syncs respectively. The
initial 512-edit reference probe is reproduced with `--iterations 512` and is
included in the 45 pin-growth samples, separately from the paired comparisons.

```sh
cargo test --release --features cli,git --lib metadata::pins::
cargo test --release --features cli,git --lib git_repository::
python3 -m unittest benchmarks.tests.test_durable_ledger \
  benchmarks.tests.test_pin_growth benchmarks.tests.test_git_import_profile \
  benchmarks.tests.test_dashboard benchmarks.tests.test_comparison \
  benchmarks.tests.test_revisions benchmarks.tests.test_all benchmarks.tests.test_cli
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```
