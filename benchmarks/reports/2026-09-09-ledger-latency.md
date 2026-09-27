# Pin-ledger latency and spare-file sync

After [deferred payload cleanup](2026-09-09-deferred-payload-cleanup.md), GC
reclaimed most obsolete logical objects during imports, but reader p99 remained
413–635 ms. This investigation separates the remaining admission costs.

## Instrumentation

GC admission now reports backend collector lease, pending pin-release barrier,
inventory read, and atomic ledger acquisition separately. Local file-ledger
operations report blocking-pool queueing, lock setup, shared/exclusive lock wait,
read/decode, transition application, encoding, total writes, capacity sync,
payload sync, and atomic exchange plus directory sync.

Ledger measurements cover all concurrent readers, writers, and GC operations
from after setup through the workload join and pending-release drain, excluding
final integrity/cleanup. They are aggregated separately from per-GC-attempt
phases: concurrent ledger events cannot be attributed to the currently running
GC attempt. Totals may overlap across threads; write subphases are nested in
write totals. Diagnostics read the clock only when their tracing target is
enabled. Instrumentation itself adds overhead, including while locks are held.

## Profiled baseline

Three sequential runs use 60 imports × 16 unique 4 KiB files, object readers,
background GC, four Tokio workers, and the existing five-minute timeout.
All three completed and passed final integrity and phase-accounting checks.

| Run | Import seconds | Reader p99 ms | Ledger write seconds | Capacity sync seconds | Payload sync seconds | Exchange/directory sync seconds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 36.80 | 677.09 | 35.19 | 16.06 | 10.14 | 8.30 |
| 2 | 61.31 | 609.70 | 59.92 | 28.36 | 16.20 | 14.58 |
| 3 | 68.40 | 695.85 | 69.47 | 33.02 | 18.75 | 16.90 |

The sync phases consume over 98% of total ledger write time in every run.
Read/decode plus encoding total only 0.86–0.89 seconds per run, and blocking-pool
queueing is small. Exclusive-lock p99 is hundreds of milliseconds, consistent
with queued operations waiting behind durable ledger writes.

| Run | GC release barrier seconds | GC inventory seconds | GC ledger acquisition seconds |
| --- | ---: | ---: | ---: |
| 1 | 3.25 | 4.25 | 3.52 |
| 2 | 8.51 | 5.51 | 6.09 |
| 3 | 9.32 | 5.16 | 7.11 |

The backend collector lease totals less than 0.3 ms per run. The release barrier
waits for pending pin operations; it is not itself a separate storage operation.
These measurements point to durable writes and the resulting lock contention,
rather than expensive inventory decoding or backend collector serialization.

## Change

Previously each Linux ledger write synced spare-file allocation, wrote the new
ledger, then synced that same file again before atomic exchange. The final
content sync already persists both allocation and contents. Remove the separate
spare allocation sync, retaining the final spare sync before exchange, the
active-slot reserve sync, and the directory sync after exchange. Initial slot
creation likewise persists allocation and contents with its final content sync.

No claim, pin, recovery, publication, locking, or persistent format changes are
made. The previous authoritative ledger remains untouched until the complete
replacement is synced. Both slots retain their durable full-disk reserve.

## Evidence

Profiled baseline artifacts are under
`benchmarks/results/online-holds-ledger-2026-09-09/`; follow-up artifacts are under
`benchmarks/results/online-holds-ledger-sync-2026-09-09/`. Each contains source
snapshots, binary hash, commands, machine-load snapshots, raw JSON, and summaries.
This shared host is not a controlled throughput environment. The follow-up
checks the structural reduction from two capacity syncs to one per ledger write,
as well as GC progress and final integrity.

## Validation

The final full all-feature suite passed 696 tests, with zero failures and 17
ignored, including slot migration, interrupted spare writes, full-disk emergency
recovery, process-crash matrices, S3, and multiprocess publication. All-feature,
all-target Clippy with warnings denied and formatting checks passed. An earlier
build was terminated by SIGTERM before tests ran; the successful retry used one
compiler job. Both logs are retained with the follow-up artifacts.

## Follow-up results

All three runs completed and passed integrity, phase-accounting, and sync-count
checks. Each recorded exactly one capacity sync and one payload sync per ledger
write, versus two capacity syncs and one payload sync in the baseline. Together
with the unchanged directory sync, this removes one of four steady-state sync
calls per ledger update (25%). There were 3,628, 3,745, and 3,686 updates in the
respective measured intervals.

| Run | Import seconds | Reader p99 ms | Ledger write seconds | Capacity sync seconds | Payload sync seconds | Exchange/directory sync seconds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 66.98 | 520.77 | 65.76 | 21.00 | 22.29 | 21.80 |
| 2 | 45.34 | 460.44 | 45.71 | 14.24 | 16.05 | 14.78 |
| 3 | 46.82 | 499.49 | 45.66 | 14.26 | 16.05 | 14.67 |

Every run reclaimed 969 of 1,003 obsolete logical objects during imports.
After-import passes removed 17, 17, and 0 objects; final cleanup removed the
remaining 17, 17, and 34. No Busy or catalog-cleanup errors occurred. All
retryable failures were stale repository revisions (16, 17, and 18), preserving
validation against concurrent publication. These are logical-object counts,
not measurements of when all retired physical representations leave disk.

Reader p99 was lower in these runs (460–521 ms versus 610–696 ms), while import
times overlap the baseline range. Individual sync latency varied substantially;
there is no reliable overall throughput improvement established by these six
shared-host runs. The confirmed improvement is one fewer durable sync per
ledger update with preserved recovery checks and active GC progress.

## Remaining bottleneck

Durable writes still serialize thousands of ledger updates. Decoding, encoding,
and blocking-pool queueing are small; bypassing inventory validation would not
address the main cost. The next substantial investigation would be reducing
syncs across independent updates, while acknowledging each acquisition only
after its protection is durable and preserving cross-process serialization,
exact-token recovery, and full-disk behavior. That requires a separate design
and cancellation/crash tests; this change does not introduce batching or remove
any remaining durability boundary.

## Integration with newer main

Before publication, main gained process-owned ordinary readers and a grouped
append journal for durable local pins (`7c9ed7b`, `feb91ad`). Those implementations
are preserved. The measurements above describe the earlier replacement-file
ledger, not the new default journal. The spare-file sync reduction now applies
to the replacement/migration path; journal durability boundaries are unchanged.
Diagnostics additionally expose group wait/total time, journal flushes, append
syncs, and checkpoints. `online_holds` still measures its experimental
`open_payload` path; the permanent `object-reads` suite covers the newer ordinary
application reader path. Final integration validation is separate from these
historical performance measurements.
