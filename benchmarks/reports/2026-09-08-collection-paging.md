# Collection paging fix — 2026-09-08

Paging retained records by physical row ID reduces median warm metadata
collection at 65,537 objects from **36.71 seconds to 1.19 seconds (−96.8%)**.
All three paired runs improved. This follows the cutoff investigation in the
[statement-cache comparison](2026-09-08-statement-cache.md).

## Cause and change

The large-repository collection path reads retained records in 256-row pages.
Its compound-key continuation predicate was:

```sql
WHERE o.namespace > ?1
   OR (o.namespace = ?1 AND o.native_id > ?2)
ORDER BY o.namespace, o.native_id LIMIT 256
```

On the pinned Turso engine, `EXPLAIN QUERY PLAN` reports a multi-index OR scan,
an indexed lookup into the retained table, and **`USE SORTER FOR ORDER BY`**.
Each page sorts the remaining joined records again. The page limit therefore
does not bound the work needed to produce each page.

The [implementation](../../src/metadata/sqlite.rs) now advances by `o.rowid`,
using `objects o NOT INDEXED` and `ORDER BY o.rowid LIMIT 256`. The first page
starts without a lower bound; later pages use `WHERE o.rowid > ?1`. This handles
negative IDs and gaps without treating IDs as offsets. The fixed plans use
a physical scan for the first page and an integer-primary-key seek thereafter,
with indexed retained-key lookups and no sorter.

Collection validation does not require canonical record order. Objects remain
stable within the write transaction until validation finishes and deletion
begins, so the physical cursor can advance safely. Root checks, unknown-key
checks, link validation, bounded reference buffers, deletion, and the collection
cutoff remain in place. The streamed implementation is extracted into a private
helper so small adversarial fixtures can test it directly.

## Isolating the query cost

A standalone probe links the same pinned Turso dependencies and reproduces the
object/retained-table indexes, with distinct blob keys and 64-byte synthetic
record values. It fetches every retained record and checks the exact key set.
Two scans share one transaction at each size. This isolates SQL paging from
record decoding, collection mutation, and checkpointing.

| Records | Original scan, two runs | Row-ID scan, two runs |
|---:|---:|---:|
| 8,192 | 468 / 574 ms | 26 / 27 ms |
| 65,537 | 52.20 / 41.40 s | 245 / 251 ms |

A tuple comparison removed the sorter at 8,192 records, but its plan still
used an index scan rather than a continuation seek; it took 87 / 78 ms.
The row-ID query was selected instead. An additional sparse-retention probe
checks the explicit `NOT INDEXED` hint and first-page plan, retaining 64 of
1,024 objects and verifying the exact result set.

## Complete metadata collection

The baseline is `e121c660be2bc0bba5150a04d78da39cef1cfa62`; the candidate adds
the paging fix and its regression tests. Both use the identical collection
probe from the prior comparison, the same lockfile, Rust 1.96.0, and optimized
library-test builds with `--release --locked --features cli`. Both build
profiles have optimization level 3 and no debug assertions.
All compilation and validation test runs finished before these measurements.

Three fresh-process pairs alternate baseline/fixed order. Each process seeds
65,537 verified leaf records in 1,024-record commits, then retains all records
in one first-use collection and one warm collection. Timing surrounds
`MetadataStore::commit`, including its normal collection maintenance. Setup
and the exact inventory/revision check after reopen are outside timing.

| Pair | Order | First before | First fixed | Warm before | Warm fixed | Warm reduction |
|---:|---|---:|---:|---:|---:|---:|
| 1 | Before, fixed | 54.479 s | 1.134 s | 36.711 s | 1.258 s | 96.6% |
| 2 | Fixed, before | 44.940 s | 1.300 s | 42.168 s | 1.190 s | 97.2% |
| 3 | Before, fixed | 40.783 s | 0.873 s | 32.132 s | 1.066 s | 96.7% |
| Median | | **44.940 s** | **1.134 s** | **36.711 s** | **1.190 s** | **96.8%** |

Median first-use duration falls 97.5%. The median warm result is about 31 times
faster. All 12 timed commits report zero removals. All six processes pass the
exact inventory and revision checks after reopening.

These are local metadata measurements on a shared host, not whole-repository
GC timings or an isolated-host latency guarantee. The fixture has no named
roots, forward links, or stale records. The regression tests cover those
correctness concerns, but the measurements do not establish the speedup for
link-heavy graphs or payload reclamation. Per-page timings, host load, and
whole-process CPU/RSS measurements are retained in the receipt; process resource
figures also include setup and verification.

## Validation and reproduction

- **412 default library tests passed**, including two new regression tests;
  14 opt-in tests remain ignored in that suite.
- The streaming-path tests cover more than two pages, sparse retained rows,
  negative physical IDs, ID gaps, mixed namespaces, a missing link after the
  first two pages, old snapshots, repeated collection, and exact reopen state.
- Separate rejection cases preserve named roots and reject unknown retained
  keys without deleting existing objects.
- **All-feature/all-target Clippy with warnings denied passed**, along with
  formatting and `git diff --check`.
- All six complete-collection benchmark invocations passed.

The [numerical receipt](2026-09-08-collection-paging.json) contains query plans,
all scan/page timings, paired collection results, binary and lockfile hashes,
the source patch, validation-log hashes, and the exact temporary probe and
runner sources. Build logs and retained artifacts are under
`benchmarks/results/2026-09-08-collection-paging/` (ignored). The baseline probe
and copied lockfile remain in the prior statement-cache results directory.

To reproduce the complete-collection run with the retained probes, use the
recorded commands for
`metadata::sqlite::tests::benchmark_statement_cache_collection`, setting
`CASITA_COLLECTION_COUNTS=65537` and `CASITA_COLLECTION_ITERATIONS=1`.
The embedded scripts contain local paths and need adaptation on another host.
