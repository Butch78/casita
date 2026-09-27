# Paired before/after reader admission

The narrower hold improves online GC progress, but the comparison does not
establish an overall performance win. The short workload pays additional reader
latency during GC. The longer workload also exposes a pre-existing fatal cleanup
error in both versions, which must be addressed before relying on that workload
for a performance or reliability claim.

## Protocol

We reused the saved, optimized binaries from the
[before](2026-09-09-merged-main-online-gc.md) and
[after](2026-09-09-narrow-reader-admission.md) investigations. SHA-256 checks
passed before and after execution. Both have base revision `c84c583`, identical
benchmark code and build features; their production difference is the temporary
reader hold. The after source also contains test-only regression code.

Each of three cases runs one excluded warmup per binary, followed by six measured
pairs. Odd pairs run before/after; even pairs run after/before. Cases are:

1. 60 imports with concurrent application reads and GC.
2. 300 imports with concurrent application reads and GC.
3. 60 imports with application reads and no concurrent GC.

Every import has 16 unique 4 KiB files. Both binaries use four Tokio workers,
the same CPU affinity (`8,10,12,14`, four distinct physical cores), local temporary
storage, and a second application-reader handle. No builds or tests from this
session run during measurement. Each invocation gets a fresh repository and
retains the benchmark's integrity, checkout, and final cleanup gates.

There are 36 measured invocations and six warmups, all retained, including
failures. Measurement ran from 15:48:37 to 16:10:07 UTC on September 9, 2026.
This was **not an exclusive or quiet host**: other sessions ran tests and the
one-minute load snapshots ranged from 3.85 to 28.18 on 16 logical CPUs. CPU
affinity fixes placement but does not isolate CPU siblings, storage, or other
resources. Alternation helps with drift; it does not remove these limitations.

## Results

Entries below are medians across successful invocations. Reader p99 is the median
of per-invocation p99 values, not a pooled percentile. The long-workload columns
have different successful subsets, so their medians are descriptive only.

| Workload | Version | Passed / attempted | Import seconds | Reader p99 ms | Obsolete logical objects removed during imports | Useful active GC passes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 60 imports + GC | before | 6 / 6 | 6.355 | 28.220 | 80.5% | 3 |
| 60 imports + GC | after | 6 / 6 | 7.435 | 44.925 | 96.6% | 54.5 |
| 300 imports + GC | before | 3 / 6 | 59.583 | 91.873 | 77.6% | 2 |
| 300 imports + GC | after | 5 / 6 | 70.132 | 134.393 | 99.3% | 279 |
| 60 imports, no GC | before | 6 / 6 | 8.302 | 37.113 | — | — |
| 60 imports, no GC | after | 6 / 6 | 7.956 | 34.711 | — | — |

For pairs where both versions pass, the median after/before ratios are:

| Workload | Complete pairs | Import duration ratio | Reader p99 ratio | Pairs with lower after import time / reader p99 |
| --- | ---: | ---: | ---: | ---: |
| 60 imports + GC | 6 | 1.126 | 1.701 | 1 / 0 |
| 300 imports + GC | 3 | 0.983 | 0.964 | 2 / 2 |
| 60 imports, no GC | 6 | 0.978 | 1.020 | 3 / 3 |

A median of pair ratios is not the ratio of the separate medians. The three
successful long pairs are survivor-conditioned and too few to establish a
timing benefit. No-GC reader-p99 ratios range from 0.394 to 2.035; neither version
consistently wins. This control does not show a consistent intrinsic reader cost.
The short-GC latency penalty is consistent in these samples, alongside much more
successful collection work. That is consistent with additional GC contention,
but the experiment does not isolate its cost from host interference.

Across successful runs, before records 52 snapshot-generation conflicts at 60
imports and 257 at 300. After records zero in both cases. At 60 imports, before
reclaims 0–93.2% during imports versus after's 96.6–98.3%. At 300, successful before
runs reclaim 45.2–83.9% versus after's 99.3–99.7%. These are logical-object counts,
not physical disk bytes freed. Physical cleanup may be deferred.

Successful invocations pass sentinel reads, fsck, byte-verified final checkout,
pin/claim cleanup, and GC phase/accounting checks. All 1,003 or 5,083 obsolete
logical objects are accounted for, including final cleanup. Failed invocations
do not emit their final JSON sample or reach all final integrity gates; their
partial timing/progress must not be treated as successful samples.

## Failure that needs attention

All four failures panic in the benchmark on a non-retryable collection error:

```text
cannot delete retired representations before catalog publication
```

Before fails in long-workload pairs 2, 4, and 5; after fails in pair 2. No warmup
or short/control invocation fails. These counts demonstrate reproduction in both
versions, not a statistically established difference in failure rates.

The guard is in `PackedChunks::finish_collection_inner` in `src/blob/pack.rs`.
It rejects cleanup when `index_dirty` or `prepared_index_catalog` indicates an
unpublished catalog. Repository collection commits the catalog, releases its
publication lock, then calls cleanup. Source inspection suggests a concurrent
writer can make the shared catalog dirty in between. The benchmark establishes
the error, not that exact interleaving; a deterministic regression is needed.

Next: reproduce that gap with a paused collector and a concurrent writer, then
make ordinary online cleanup safely defer or retry while preserving publication
and deletion protection. Do not remove the guard or classify the benchmark
failure as success. Rerun the longer case after the fix.

The narrower hold remains justified by its precise protection semantics and
improved GC progress. These results do not justify claiming it makes the whole
workload faster or that the current GC implementation is ready without the
cleanup-race follow-up.

### Deterministic investigation follow-up

`blob::pack::tests::writer_after_gc_catalog_commit_reproduces_cleanup_publication_error`
now reproduces the offending backend interleaving without timing or injected
errors. Four cases cover memory/local storage and dirty/prepared writer state:

1. Publish live and dead chunks in one pack.
2. Remove the dead chunk, compact the pack, and successfully publish GC's catalog.
   Assert both unpublished-state indicators are clear and the old pack awaits cleanup.
3. Stage and flush a new writer upload on the shared backend, with its new pack
   protected in the pin ledger. Optionally prepare its catalog without publishing.
4. Invoke ordinary pinned cleanup. Both the dirty and prepared states reproduce
   the exact benchmark error, despite GC's own catalog already being committed.
5. Verify that no deletion claim was created and both the retired and new packs
   remain present. Publish the writer's catalog and retry cleanup: the old pack
   is reclaimed, live and new bytes remain readable, and ledger ownership settles.

All four cases pass. This confirms that a later writer can trigger the shared
publication guard after GC's commit; it is a backend-level deterministic sequence,
not a paused end-to-end repository scheduler test. The tested failure does not
delete data. The error is a plain `io::Error::other`, giving `Unknown` retry
guidance, which the benchmark correctly treats as a failure rather than a retry.

Production behavior is unchanged by this investigation. The fix should preserve
the publication safety check while allowing pinned online cleanup to defer work
when a later writer is unpublished. A follow-up regression should require that
behavior and cover eventual reclamation and interrupted publication. Simply
removing the guard would weaken safety; holding the publication lock across slow
cleanup could unnecessarily stall writers.

## Permanent coverage and reproduction

The existing `online-holds` case remains registered in `benchmarks/manifest.json`.
Its `benchmark all --profile standard` matrix now covers 60 and 300 imports,
16 files per import, and all four import/reader/GC combinations. Smoke remains
3 imports × 2 files. The runner explicitly selects application readers and all
scenarios, preventing inherited experimental environment settings from silently
changing corpus coverage.

The six benchmark-runner tests pass, including the standard size matrix and
preservation of samples when a later scenario fails. An actual corpus smoke
passes all four application scenarios despite deliberately inherited snapshot
and single-scenario settings. No production code changes were made for this
comparison; the earlier 766-test and Clippy results remain applicable to the
measured narrower-hold source.

Reproduce one invocation for either saved binary:

```sh
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 \
CASITA_BENCH_READER_SCOPE=application CASITA_BENCH_SCENARIO=imports_readers_gc \
taskset -c 8,10,12,14 /path/to/saved/online_holds
```

Use 300 imports for the longer case or `imports_readers` for the no-GC control.
The exact alternating runner, summary script, protocol, artifact hashes, host
snapshots, all 42 raw logs/results, and corpus validation output are retained in
`benchmarks/results/paired-reader-online-gc-2026-09-09/`. The runner exits nonzero
because it preserves the four failed measurements. The prior result directories
retain the executable bytes and corresponding source patches.
