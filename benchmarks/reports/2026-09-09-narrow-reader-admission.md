# Narrow application-reader admission

Ordinary object opens now protect the requested logical closure during admission
instead of temporarily retaining every object in a snapshot. The candidate
catalog and metadata resources remain pinned until the final reader has acquired
its physical read plan. Revision/resource revalidation remains unchanged, as do
explicit retained-reader snapshot semantics.

## Correctness

A deterministic eight-case regression pauses opening immediately after admission:
memory/local storage × already marked/unmarked object × completion/cancellation.
The local cases use independently opened reader and collector handles. An older
GC plan collects unrelated garbage when the requested object was already marked,
but rejects pruning when the new pin protects an existing unmarked object.
Successful reads survive the handoff; cancelled opens release protection. Final
collection leaves no pins, deletion claims, collector, or prune fence.

The full all-feature suite passes **766 tests, with 28 ignored**, including existing
missing-object, transitive-closure, catalog-replacement, retained-reader, and
process-crash tests. All-feature/all-target Clippy with warnings denied, formatting,
and diff checks pass.

## Measurements

Three sequential runs use the permanent `online-holds` application-reader case:
60 generic filesystem imports × 16 unique 4 KiB files, one reader on a second
repository handle, concurrent GC, and four runtime workers. Compilation and tests
finished before measurement. Compare the previous
[merged-main application runs](2026-09-09-merged-main-online-gc.md).

| Run | Import seconds | Reader p99 ms | Removed during imports | After imports / final cleanup | Useful active GC passes | Snapshot-generation conflicts | Stale-revision retries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 5.346 | 44.930 | 969 | 17 / 17 | 54 | 0 | 3 |
| 2 | 5.073 | 66.139 | 986 | 17 / 0 | 53 | 0 | 5 |
| 3 | 4.892 | 61.351 | 969 | 17 / 17 | 53 | 0 | 4 |

Every run passes sentinel reads, fsck, byte-verified checkout, final pin/claim
cleanup, and GC attempt/phase accounting. Removals account for all 1,003 obsolete
logical objects. No Busy or catalog-cleanup errors occur; remaining retryable
errors are stale revisions from concurrent publication.

Active GC reclaims 97–98% of obsolete logical objects, versus 78–85% previously,
and useful active passes rise from 5–7 to 53–54. Reader p99 is higher than the
previous 29–33 ms. These short shared-host runs establish the observed GC progress
and conflict removal, not a controlled latency or throughput improvement. Removal
counts describe logical objects, not physical bytes freed; physical cleanup may
be deferred by protection.

| Run | Journal append syncs | Append-sync seconds | Checkpoints | Checkpoint seconds | Group-wait p99 ms | Exclusive-lock p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1038 | 2.979 | 27 | 0.151 | 13.479 | 5.652 |
| 2 | 1045 | 2.751 | 28 | 0.142 | 11.738 | 4.667 |
| 3 | 1048 | 2.608 | 28 | 0.141 | 11.736 | 4.597 |

Sync and lock costs remain substantial, but concurrent timings overlap and are
not additive parts of import wall time. Group wait includes queueing, execution,
and reply; append counts exclude checkpoint, capacity-growth, and adoption syncs.
More successful GC passes also perform more work, so the latency change cannot
be assigned to the reader hold alone.

## Reproduction and evidence

The case is registered in `benchmarks/manifest.json` and included in `benchmark
all`; application mode is its default. Reproduce one repetition with:

```sh
CARGO_BUILD_BUILD_DIR=/tmp/casita-online-holds-build \
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 \
CASITA_BENCH_READER_SCOPE=application CASITA_BENCH_SCENARIO=imports_readers_gc \
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' \
  bench --features experimental --bench online_holds
```

`benchmarks/results/narrow-reader-online-gc-2026-09-09/` retains the executable,
its verified SHA-256, base revision and source patch, build/test/Clippy logs,
runner and summary scripts, platform/load snapshots, raw output, and JSON results.
Measurements use uncommitted changes on `c84c583`.
