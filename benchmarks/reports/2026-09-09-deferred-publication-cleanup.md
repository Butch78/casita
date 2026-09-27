# Defer online cleanup across a newer catalog publication

Pinned online payload cleanup now leaves retired paths queued when the shared
catalog has pending changes from a writer. The collector can finish normally;
a later pass reclaims those paths after publication succeeds. No deletion occurs
in the deferred branch. Unpinned cleanup and cleanup under an emergency logical
prune fence retain the publication error.

This addresses the failure reproduced in both binaries during the
[paired reader comparison](2026-09-09-paired-reader-admission.md). A writer could
stage or prepare its next catalog after GC had published its own catalog but
before GC reached cleanup. The shared dirty/prepared flags then caused an
unnecessary fatal error. The change preserves the publication guard rather than
deleting against unpublished state or holding writers until cleanup completes.

## Regression and validation

The investigation test is now named
`online_cleanup_defers_new_publication_and_reclaims_after_retry`. Its new success
expectation first failed on the old code with the exact benchmark error. With
the fix, its memory/local × dirty/prepared cases verify:

- Ordinary collection and vacuum defer pending cleanup without deleting either
  the retired pack or the writer's new pack.
- The collector can release ownership while publication remains pending.
- Aborting a prepared publication restores dirty mutations; cleanup on a new
  pass continues to defer until publication retries successfully.
- After successful publication, cleanup removes the old pack, preserves live
  and new bytes, and leaves no pins, claims, collector, or prune fence.
- Unpinned cleanup and an active emergency prune fence still reject unpublished
  catalogs; the emergency fence remains installed until explicitly released.

The full all-feature suite passes **767 tests, with 28 ignored**. All-feature,
all-target Clippy with warnings denied, formatting, and diff checks pass.

## Longer workload follow-up

Three sequential runs use the permanent application-reader benchmark, each with
300 imports × 16 unique 4 KiB files, one reader on a second repository handle,
and concurrent GC. Both the runtime worker count and CPU affinity match the
paired investigation: four workers on CPUs `8,10,12,14`. Build and tests finished
before measurement. These runs have no separate warmup and run on a shared host;
they verify completion and integrity, not a controlled performance improvement.

| Run | Import seconds | Reader p99 ms | Logical objects removed during imports | After imports / final cleanup | Useful active GC passes | Stale-revision retries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 31.248 | 72.330 | 5049 | 17 / 17 | 235 | 60 |
| 2 | 50.856 | 101.448 | 5066 | 17 / 0 | 259 | 38 |
| 3 | 57.247 | 113.574 | 5049 | 17 / 17 | 260 | 37 |

All three runs pass sentinel reads, fsck, byte-verified checkout, final pin/claim
cleanup, and GC attempt/phase accounting. Each accounts for all 5,083 obsolete
logical objects. Active collection removes 99.3–99.7%; no Busy results or
catalog-publication failures occur. Remaining retryable errors are stale
revisions from concurrent publication. Logical-object removal does not measure
physical bytes reclaimed, and deferred cleanup remains intentional.

Three successful runs do not prove the absence of all concurrency bugs. The
deterministic regression verifies this specific interleaving; the full suite
covers the existing recovery, cancellation, and retained-data behavior.

## Reproduction and evidence

The `online-holds` case remains registered in `benchmarks/manifest.json`, and
`benchmark all --profile standard` includes both 60 and 300 imports with all
four reader/GC combinations. Build once, then reproduce a measured invocation:

```sh
CARGO_BUILD_BUILD_DIR=/tmp/casita-online-holds-build \
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' \
  bench --features experimental --bench online_holds --no-run

CASITA_BENCH_IMPORTS=300 CASITA_BENCH_FILES=16 \
CASITA_BENCH_READER_SCOPE=application CASITA_BENCH_SCENARIO=imports_readers_gc \
taskset -c 8,10,12,14 /path/to/built/online_holds
```

`benchmarks/results/deferred-publication-cleanup-2026-09-09/` retains the exact
runner, source patch and base revision, verified executable/hash, build and
validation logs, red regression log, raw benchmark logs/JSON, host load snapshots,
and summary script/output. Measurements use uncommitted changes on `c84c583`.
