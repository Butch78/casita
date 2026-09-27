# Ordered metadata inventory investigation — 2026-09-08

The ordered snapshot API still sorts the remaining candidates on every 256-row continuation page. The new `metadata-scan` corpus entry measures the public API, verifies exact records in canonical order, and retains query plans.

On production revision `bd4aded`, warm scanning grew from 145.9 ms at 8,192 objects to 13.95 s at 65,537 objects: 95.6× the time for approximately 8× the objects. This is a local diagnostic scaling run, not a controlled revision comparison.

## Measurements

One fresh process per size, one first scan and one warm scan. Fixtures contain verified blobs inserted in batches of 1,024, then reopened. Timing includes consuming the complete record stream into a vector; seeding, reopen, record equality checks, and query-plan collection are outside timing. First does not imply a cold OS cache. Process RSS includes fixture and audit allocations.

| Objects | First scan ms | Warm scan ms |
|---:|---:|---:|
| 256 | 0.683 | 0.405 |
| 257 | 0.885 | 0.505 |
| 8,192 | 161.430 | 145.924 |
| 65,536 | 14053.038 | 16689.092 |
| 65,537 | 14288.626 | 13949.297 |

All five final cases passed exact ordered-record and revision checks. The final smoke run also passed through `benchmark all` at 256, 257, and 8,192 objects. The collection suite passed its cutoff smoke cases after rebasing onto the online-pin changes.

The shared host was not isolated. Earlier attempts varied substantially, so these numbers should not become tight clock-based test thresholds. The query plans and repeated work explain the observed scaling; absolute latency needs controlled measurements.

## Query plans and next change

### or-continuation

```text
MULTI-INDEX OR objects (sqlite_autoindex_objects_1, sqlite_autoindex_objects_1)
USE SORTER FOR ORDER BY
```

### tuple

```text
SCAN objects USING INDEX sqlite_autoindex_objects_1
```

### namespace-range

```text
SEARCH objects USING INDEX sqlite_autoindex_objects_1 (namespace=? AND native_id>?)
```

The current OR predicate uses a sorter. The tuple alternative reports an index scan. The namespace-specific alternative reports a bounded search on both key components. These alternative plans are diagnostics; neither alternative is an implemented or timed API scan.

The next change is to seek within the current namespace, then advance explicitly to the next namespace when that range is exhausted. Preserve canonical key order and bounded pages, and test namespace transitions, empty ranges, multiple pages, and retained snapshots before measuring the implementation. Paging the ordered API by physical row ID would not preserve its contract.

## Diagnostic teardown failure

The initial probe opened an additional read transaction only for EXPLAIN. Two attempts hit Turso’s `reader slot released by non-owner` assertion during connection teardown, after scan data had been emitted. The runner rejected the failed processes and marked the matrix incomplete. Those receipts are retained under `excluded_diagnostic_attempts` and are not used in the table.

The final probe releases the measured snapshot and uses the existing serialized database connection for EXPLAIN. The final matrix passed. This is a benchmark lifecycle correction, not a claim to have fixed Turso’s reader-coordination assertion for arbitrary overlapping transactions.

## Reproduction and validation

```console
benchmark run metadata-scan --profile standard --iterations 1 --repetitions 1 --output benchmarks/results/metadata-scan.json
benchmark all --profile smoke --suites metadata-scan --repetitions 1 --output benchmarks/results/metadata-scan-smoke
```

The suite is registered for direct execution, all-suite runs, revision comparisons, and dashboard normalization. The raw receipt embeds the exact probe sources, binary identities, environment, query plans, passing runs, and rejected diagnostic attempts.

Validation: 151 benchmark harness tests passed; all-feature/all-target Clippy and formatting passed. After rebasing the collection fix, the library run passed 472 tests and timed out in one catalog-rebase crash test. Its isolated retry passed all 81 crash checkpoints, completing validation of all 473 enabled library tests. The logs preserve the original timeout and retry rather than reporting an uninterrupted green run.

[Machine-readable receipt](2026-09-08-ordered-scan.json).
