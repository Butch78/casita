# Bounded collector admission retries

Collector admission now retries up to 32 times if an incidental pin update
changes the ledger revision before ownership is acquired. Each attempt still
uses the exact original expected collector token. Seeing a different collector
ends admission immediately; retries never adopt another owner's authority.

The change lives inside the tracked `CollectorLease::try_acquire` task.
Cancellation cleanup and the low-level conditional ledger operation remain
intact. Both logical/full collection and catalog-only maintenance use this
helper. Admission happens before metadata marking; refreshing its revision
does not reuse a stale reachability result.

## Test evidence

The deterministic admission regression failed before this change:
after duplicate pin churn, the helper returned None instead of acquiring.
With retries, the same test passes on memory, file, and object-store ledgers.
It also checks that another collector cannot acquire ownership by simply
refreshing the revision.

110 repository and pin-ledger tests passed, including collector recovery and
emergency/full-disk paths.

## Benchmark

Command:

```sh
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' bench --features experimental --bench online_holds
```

Default object readers, 30 imports × 16 unique 4 KiB files. All four scenarios
completed with final collection, leak checks, fsck, and byte-verified checkout.

| Metric | Imports + GC | Imports + readers + GC |
| --- | ---: | ---: |
| Admission Busy failures | 0 | 0 |
| Changed-pin mark Busy failures | 9 | 27 |
| Catalog-pin retryable failures | 26 | 0 |
| Stale metadata revision failures | 3 | 2 |
| Successful passes during imports | 5 | 4 |
| Reported objects removed during imports | 136 | 0 |
| Successful passes after imports | 1 | 0 |
| Reported objects removed after imports | 0 | 0 |
| Final cleanup objects removed | 323 | 493 |

The preceding combined run had 101 admission failures and 12 changed-pin
mark failures, with no successful passes. This run had no admission failures
and reached four successful passes during imports, but they reported **zero
objects reclaimed**. All 493 obsolete objects remained for final cleanup.

This fixes the observed admission obstacle, not continuous reclamation.
Zero admission Busy results mean no attempts exhausted the retry budget;
the benchmark does not count successful internal retries.

The no-reader run's 136 reported removals plus 323 cleanup removals fall
short of 493 because retryable passes may already have made partial progress.
Do not treat successful-pass removal totals as a complete deletion audit.

Machine load differed substantially between runs. This is evidence about
observed conflict types and progress, not a controlled throughput comparison
or proof of progress under every schedule.

## Next investigation

Changed protection now accounts for every Busy failure in the combined run.
The earlier memory-backed reproduction shows that adding only unpublished
physical bytes can invalidate a logical mark even without changing metadata
or logical object protection.

Logical prune validation currently compares complete payload protections,
including catalogs and physical resources. Full collection also builds the
physical inventory before executing logical pruning, extending the conflict
window.

The next candidate is to separate logical protection validation from physical
resource validation for ordinary pruning. New logical object/closure/snapshot
protection must still invalidate or extend the logical mark, and metadata
revision checks must remain. Physical sweep and catalog reclamation must
continue consulting their own pins and deletion fences. Emergency collection
requires separate care because it may reclaim physical data before metadata
pruning succeeds.

The meaningful benchmark target is objects reclaimed during imports, not
merely successful zero-removal passes.

## Raw evidence

`benchmarks/results/online-holds-admission-2026-09-08/` contains the log,
parsed measurements, executable hash, environment snapshot, and tracked diff.
Reason totals and during/after totals were checked for all four scenarios.

Final validation: all five online-GC, contention, and cancellation integration
tests passed, along with all-feature/all-target Clippy with warnings denied,
formatting, and diff checks.
