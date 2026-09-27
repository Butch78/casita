# Reusing the pin index during journal checkpoints

The reference is `fd2f363`. The candidate changes only checkpoint handling and
adds a regression test. Both release binaries include the same permanent
benchmark harnesses; each report records its binary SHA-256.
Measurements precede integration with `3a78302`, including the separate
mutation-catalog retention fix and pinned Turso WAL dependency. Those later
changes are not part of this measured before/after comparison.

Previously, a cached checkpoint cloned the inventory before writing, then
cloned it again and rebuilt its resource index afterward. The candidate writes
the existing inventory and retains its validated index. It updates the epoch,
cursor and operation count only after the file and parent-directory durability
barriers succeed. Legacy activation and explicit inventory replacement still
construct an index. The journal format, checkpoint thresholds and write/sync
sequence are unchanged.

The shared checkpoint writer retains every existing crash-injection boundary.
The added regression exercises two byte-triggered checkpoints, duplicate
additions, shared storage/catalog/metadata resources and an outstanding deletion
claim. It checks arbitration before cache eviction, exact encoded-size tracking,
disk replay and successful deletion after both owners release their pins.

## Reproduction

These runs use existing permanent suites registered in
`benchmarks/manifest.json` and included in `benchmark all`. The pin-growth cases
at 8,192 and 16,384 resources cover both sides of the 1 MiB journal window; the
one-resource case is a control. Each repetition measures 16 sequential additions
after seeding the pin, then discards the cache and checks exact replay,
protection, release and deletion. Import cases check independent Git inventories,
refs, persisted roots and full closure verification.

Build each revision with:

```sh
cargo test --release --features cli,git --lib --no-run
cp /path/to/emitted/libtest /tmp/checkpoint-REVISION
```

Run each command for both saved binaries, using distinct output paths:

```sh
benchmark run pin-growth --profile standard --repetitions 3 \
  --probe-binary /tmp/checkpoint-REVISION --no-build --output /tmp/pins.json
TMPDIR=/dev/shm benchmark run pin-growth --profile standard --repetitions 3 \
  --probe-binary /tmp/checkpoint-REVISION --no-build --output /tmp/pins-tmpfs.json
TMPDIR=/dev/shm benchmark run git-import-profile --profile smoke --repetitions 3 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /tmp/checkpoint-REVISION --no-build --output /tmp/import.json
TMPDIR=/dev/shm benchmark run git-import-profile --profile standard \
  --shapes delta-heavy --repetitions 1 \
  --fixture-root /tmp/casita-git-profile-fixtures \
  --probe-binary /tmp/checkpoint-REVISION --no-build --output /tmp/import-large.json
```

Measurements run sequentially, outside our builds, on a shared Ryzen 7 7840S
Linux host. Other workloads can affect timings. Btrfs and RAM-backed destination
results are retained separately: tmpfs removes disk flush latency but keeps the
same calls to durability barriers. It measures CPU and memory-filesystem costs,
not durable-disk performance. Source setup, repository opening and closure
audits are excluded from import timing. The standard case has one repetition
per binary and should be treated as a larger-scale validation sample.

Git ingestion defaults remain 16 active objects and a 64 MiB decoded-source-body
budget. Source pack caching is disabled in these benchmarks.

## Results

Pin-growth medians in milliseconds per addition, three repetitions of 16
additions each. Because host load changed during the build, the reference was
repeated after the candidate; both reference runs are retained.

| RAM-backed resources | Reference | Candidate | Repeated reference |
|---:|---:|---:|---:|
| 1 | 0.091 | 0.286 | 0.357 |
| 8,192 | 10.863 | 4.145 | 7.175 |
| 16,384 | 40.357 | 6.114 | 29.032 |

The larger cases improve against both reference batches. At 16,384 resources,
the repeated reference is 4.75 times the candidate's latency. The one-resource
case does not checkpoint and exposes scheduling noise in the measurements.
See [reference](pin-growth-tmpfs-before.json),
[candidate](pin-growth-tmpfs-after.json) and
[repeated reference](pin-growth-tmpfs-reference-repeat.json).

| Btrfs resources | Reference | Candidate | Repeated reference | Repeated candidate |
|---:|---:|---:|---:|---:|
| 1 | 6.123 | 14.228 | 9.341 | 1.468 |
| 8,192 | 22.157 | 51.467 | 23.974 | 6.757 |
| 16,384 | 35.617 | 99.066 | 48.452 | 13.740 |

The first candidate disk batch was slower, including the control that never
checkpoints. These measurements do not establish a durable-disk improvement;
the negative results are retained in [candidate](pin-growth-after.json), with
[reference](pin-growth-before.json) and
[repeated reference](pin-growth-reference-repeat.json). A final
[candidate repeat](pin-growth-candidate-repeat.json), after the large import,
was faster than either reference. This reversal, also visible in the unchanged
one-resource control, shows why these disk batches cannot isolate the change.
Repeat runs use the same commands and settings with separate output names.
All journal byte,
checkpoint and sync counts match exactly between binaries on both filesystems.

All 54 smoke imports passed their independent audits, and corresponding roots
and object counts match exactly. RAM-backed initial-import medians in seconds:

| Shape | Reference | Candidate |
|---|---:|---:|
| Many objects | 0.186 | 0.053 |
| Delta-heavy | 0.197 | 0.067 |
| Wide tree | 0.824 | 0.218 |

The raw [reference](import-tmpfs-before.json) and
[candidate](import-tmpfs-after.json) also retain unchanged and incremental
operations. Unchanged imports improve despite bypassing the changed path, so
host effects contribute to these differences; the table alone does not isolate
the checkpoint optimization's effect.

The standard delta-heavy import contains 14,060 initial objects representing
2,118,555,482 logical bytes. Both binaries passed all three audits and preserved
identical roots and object counts. RAM-backed measurements, one repetition:

| Operation | Reference, seconds | Candidate, seconds | Reference RSS, MiB | Candidate RSS, MiB |
|---|---:|---:|---:|---:|
| Initial | 184.308 | 60.784 | 1,113.4 | 709.6 |
| Unchanged | 0.023 | 0.023 | 59.0 | 52.4 |
| Incremental | 0.387 | 0.342 | 232.1 | 186.5 |

The initial import took 67% less wall time in this comparison. Peak RSS
is measured immediately after import, before the independent closure audit,
and includes repository opening. See [reference](import-standard-tmpfs-before.json)
and [candidate](import-standard-tmpfs-after.json). This is a single shared-host
comparison on tmpfs, not a general disk-performance claim.

## Validation

All 69 pin tests and six Git repository tests passed in the release build.
The Python benchmark/dashboard/comparison checks ran 66 tests, with 65 passing
and one existing skip. Clippy passed with all features and targets and warnings
denied. Formatting and whitespace checks passed.
All 60 import samples and 63 pin-growth samples passed their correctness gates;
all 11 JSON reports normalize successfully for the dashboard. Every paired
import root/object count and every pin-growth durability metric matches.

```sh
cargo test --release --features cli,git --lib metadata::pins::
cargo test --release --features cli,git --lib git_repository::
python3 -m unittest benchmarks.tests.test_pin_growth \
  benchmarks.tests.test_git_import_profile benchmarks.tests.test_dashboard \
  benchmarks.tests.test_comparison benchmarks.tests.test_revisions \
  benchmarks.tests.test_all benchmarks.tests.test_cli
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```
