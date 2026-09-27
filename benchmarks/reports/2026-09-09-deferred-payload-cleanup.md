# Defer contended payload cleanup

The [previous admission benchmark](2026-09-09-atomic-collector-and-claims.md)
eliminated collector admission failures and chunk-page retries, but each run
still aborted 78–80 collection attempts because catalog pins changed during
payload cleanup. Those failures kept retired pin history alive across retries.

## Change and safety boundary

A changed payload-pin snapshot now has a distinct internal error when the exact
collector still owns the ledger and no logical-prune fence is active. This
outcome is produced before the affected batch acquires a claim or starts I/O.
Ordinary post-publication payload cleanup catches only this specific outcome,
stops scanning, and leaves remaining retired paths queued for a later pass.
Completed logical and physical sweeps can then finish their claims and collector
ownership, releasing retired pin history.

Lost collector ownership, active prune fences, conflicting deletion claims,
storage failures, and claim-finalization failures still propagate. Emergency
retirement and catalog-object reclamation retain their strict behavior. The
change does not broadly suppress `WouldBlock` errors or weaken deletion claims.
Tracked deletion tasks continue through caller cancellation.

## Regression coverage

- A storage-path hold acquired after marking survives a deferred pass and
  collector completion; retired history is cleared while the active hold stays.
  A subsequent pass preserves held bytes, then reclaims them after release.
- Historical catalog holds acquired after marking preserve old packed bytes
  across collector completion, for both inline and sharded catalogs.
- A stale mark under an active prune fence still fails.
- Lost ownership and injected deletion failure still fail; an outstanding
  claim blocks collector completion and requires exact owned-claim recovery.
- Cancelling a caller during a paused deletion retains its claim until the
  tracked operation finishes; queued retired paths remain retryable.

## Measurement

Three sequential runs use the same combined workload as the previous report:
60 imports × 16 unique 4 KiB files, object readers, background GC, four runtime
workers, and the unchanged five-minute scenario timeout. Builds and tests
finish before measurement. The shared host makes wall-clock comparisons
diagnostic rather than controlled throughput measurements.

Artifacts are in `benchmarks/results/online-holds-deferred-2026-09-09/`:
source snapshot, binary hash, commands, test/build logs, machine-load snapshots,
and per-attempt phase timings. Successful runs also verify fsck, reader bytes,
checkout bytes, and final pin/claim cleanup. Reported removals count successful
passes by completion time; they do not timestamp individual object deletion.

## Validation

The full all-feature suite passed 696 tests, with zero failures and 17 ignored.
All-feature/all-target Clippy with warnings denied and formatting checks passed.
The initial cancellation test used the pin-release barrier instead of the
repository task barrier and asserted before deletion settled. After correcting
that test synchronization, the full suite passed; no production change was
needed for that failure.

## Results

All three repetitions completed and passed integrity and phase-accounting
checks. No catalog-cleanup error or Busy admission failure occurred.

| Run | Import seconds | Removed during imports | Removed after imports | Final cleanup | Useful passes during imports | Reader p99 ms | Stale-revision retries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 24.61 | 969 | 17 | 17 | 52 | 412.54 | 5 |
| 2 | 54.53 | 952 | 34 | 17 | 45 | 587.07 | 12 |
| 3 | 60.39 | 969 | 0 | 34 | 44 | 635.38 | 14 |

Every run accounts for all 1,003 obsolete logical objects. Successful passes
reclaimed 94.9–96.6% during imports, compared with 17 objects per run in the
previous experiment. Useful pass completions span 3.02–24.42 seconds,
0.44–52.55 seconds, and 2.75–59.32 seconds respectively. This demonstrates
repeated reclamation throughout this workload, rather than a single pass
finishing after imports stop. Counts describe logical-object reclamation;
deferred physical representations may remain until subsequent cleanup.

The longest individual GC attempts were 1.37, 1.52, and 1.83 seconds. Chunk
claim attempts/pages were 53/53, 48/46, and 46/44: four total refresh retries,
without the earlier multi-minute claim-admission loop. All remaining retryable
errors were stale repository revisions caused by concurrent publication.

Import times and reader p99 were lower than the previous shared-host runs
(137–186 seconds and 1,577–2,584 ms), but this is not a controlled speedup
measurement. The stronger result is successful active reclamation and complete
object accounting in every repetition.

## Remaining work

Admission and ledger work still contribute substantial latency. Collector
admission totals 8.77, 15.50, and 17.74 seconds across the runs; this phase also
includes the collector lease, pending-release barrier, and inventory work.
Logical marking totals 2.76, 7.99, and 8.48 seconds, and payload cleanup totals
3.79, 7.50, and 9.29 seconds. Nested phase times must not be added twice.

Reader p99 remains 413–635 ms. A useful next investigation is to split durable
ledger/lease admission costs further and check whether repeated inventory work
can be reduced without changing protection. Stale-revision retries remain a
correct response to concurrent publication; these results do not justify
weakening that validation. Sustained disk-space reclamation under prolonged
contention, beyond this benchmark's successful logical-object accounting and
final cleanup, also remains a separate measurement.
