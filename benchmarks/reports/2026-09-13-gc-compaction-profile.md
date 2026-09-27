# Local GC compaction profile

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

This investigation adds optional nested timings to the existing permanent
`held-catalog-gc` workload. It changes no compaction decisions, storage ordering,
hold semantics, or public API. The timing helper is now crate-visible so pack
compaction can use the same disabled-by-default tracing target as repository GC.

## Results

All **24 samples passed**, including their after-release collection. Marker writes
are the dominant `finish_deletions` activity with eight holds. The 1,024-blob case
has nine packs to compact (two initial packs and seven later publications), so it
writes nine markers and one replacement pack. The 128-blob case has eight markers.
One-hold controls have one and two markers respectively, and one replacement pack.

Median milliseconds (six repetitions per row):

| Blobs | Holds | Whole GC | Finish deletions | Marker writes, busy | Replacement write, busy | Copy/hash, busy |
|---:|---:|---:|---:|---:|---:|---:|
| 128 | 1 | 239.09 | 125.47 | 58.31 | 50.57 | 1.33 |
| 128 | 8 | 561.80 | 317.81 | 317.56 | 49.89 | 0.64 |
| 1,024 | 1 | 473.91 | 104.15 | 100.96 | 41.15 | 13.90 |
| 1,024 | 8 | 470.63 | 222.51 | 221.96 | 42.06 | 6.40 |

The median per-sample marker-busy / finish-deletions ratio is **99.95%** for
128/eight and **99.86%** for 1,024/eight. This means a marker write is outstanding
for almost the entire interval; it does not prove all that time would disappear
if marker writes became free. Replacement writes and CPU work overlap those waits.
For 128/one, replacement and marker writes run sequentially and both matter.
Loading medians range from 0.41 to 3.69 ms; classification from 0.03 to 0.83 ms.
Neither is the main cost in these cases.

Whole-GC ranges were 161.73–581.54 ms (128/one), 284.59–1,974.86 ms (128/eight),
240.59–5,294.26 ms (1,024/one), and 355.91–869.96 ms (1,024/eight). These broad,
non-monotonic results show why this run cannot establish normal latency or be
compared directly with the earlier uninstrumented run. No new performance cliff
was established; both existing size and hold controls remain in the corpus.

[Raw measurements](2026-09-13-gc-compaction-profile.json) retain every sample.

## Recommendation

Next, prototype bounded batching of durable local marker writes using the existing
local durability machinery, then benchmark against the unchanged implementation.
Keep the on-disk marker format and recovery semantics initially. This profile
justifies investigating the write path; it does not establish that batching will
help or how much it will save, especially with markers in distinct directories.
An alternative is a narrower syscall profile to separate file/directory syncs
from scheduling and pin waits before attempting a runtime change.

Source inspection confirms `compact_pack` uses `put_object` through the pinned
object-store wrapper to `LocalFileSystem.with_fsync(true)`. Its successful local
PUT syncs the file and destination parent; creating missing shard directories
also requires directory syncs. These are durable writes, but no claim here assigns
a measured fraction specifically to fsync. The immutable HTTP attribute attempt
is rejected by the local backend before falling back to its ordinary PUT.

Do not simply remove markers: `reclaim_retired_packs` replays them after crashes,
and index fallback reconstruction uses them to identify superseded packs. Any
batching experiment must preserve durability before catalog publication, retry
behavior after partial writes, emergency-space ordering, and held-pack protection.
There is no new runtime optimization in this investigation.

## Method

The release fixture runs 128 and 1,024 deterministic 4 KiB blobs with one and eight
selected historical holds, six repetitions per case. Each repetition creates a
fresh repository and checks exact logical removals, preservation of all historical
pack files, complete reads through every held session, and zero pack bytes after
release. Setup and correctness checks are outside the measured GC intervals.
See the [whole-GC fixture report](2026-09-13-held-catalog-gc.md) for workload details.

Nested phases cover pack loading, classification, copying and hash validation,
sealing, retirement, replacement PUTs and marker encoding/PUTs. At most two packs
compact concurrently. Per-phase `busy_seconds` is the union of its measured wall
intervals, while `summed_seconds` adds individual durations. Different phases can
overlap, and outer phases contain inner phases; do not add their busy times.
Tracing event delivery supplies interval endpoints, so these are approximate
wall intervals rather than a CPU or syscall profile. PUT times include scheduling,
pin protection and storage waits; they do not isolate fsync latency.

The host is shared and unrelated builds were active. Our build and tests finish
before accepted samples. Absolute timings may vary; this is an attribution study,
not a before/after optimization or an overhead-ceiling certification.

## Reproduce

```console
cargo test --locked --offline --release --features cli --lib --no-run -j 2
benchmark run held-catalog-gc --profile standard --repetitions 6 --probe-binary LIB_TEST_EXECUTABLE --no-build --output compaction-profile.json
benchmark all --suites held-catalog-gc --repetitions 1 --bin-dir BIN_DIRECTORY --output /tmp/compaction-smoke
python3 -m unittest discover -s benchmarks/tests
```

The binary directory for `benchmark all` must contain `casita-lib-test`. The suite
is registered in `benchmarks/manifest.json` and included in `benchmark all`.
The raw report retains artifact/source hashes, filesystem/environment details,
all phase events, derived overlap-aware summaries and full probe output.

## Validation

- Release build completed; its only warning was the pre-existing unused
  `CountingCompactState` test helper under the CLI feature set.
- 24/24 measured samples passed all live-GC and after-release correctness gates.
- 94 pack regression tests passed (15 ignored benchmark/test probes).
- 245 Python benchmark tests passed, including new overlapping/nested interval
  accounting tests and compatibility with older binaries without compaction phases.
- The registered `benchmark all --suites held-catalog-gc` smoke run passed.
- `cargo fmt --all -- --check` and `git diff --check` passed.

### Reproducing after the later per-marker change

The later [individual-marker comparison](2026-09-13-gc-local-marker-put.md) changed
local marker publication. To reproduce this report's earlier runtime from the
current source, first apply `2026-09-13-gc-local-marker-put-baseline.patch` in an
isolated copy, then follow this report's commands and any experiment-specific patch.
