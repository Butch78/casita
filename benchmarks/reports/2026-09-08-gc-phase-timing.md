# GC phase timing under concurrent imports and readers

This follows the [atomic-prune-admission measurements](2026-09-08-atomic-prune-admission.md).
The change adds optional debug timing events; it does not change GC admission,
retention, or deletion rules. Without the timing target enabled, phase guards
skip clock reads. The online-holds benchmark installs a subscriber for that
specific target and includes phase events in every GC attempt, including failed
attempts. Normal reader/writer trace events are filtered out.

## Measurement

Three sequential combined-workload runs: 60 imports × 16 unique 4 KiB files,
object readers, four runtime workers, and the existing five-minute timeout.
Compilation and tests finish before measurement. The machine is shared, so
these are diagnostic timings rather than controlled throughput comparisons.

`gc_attempts` records start/finish timestamps, outcome, and phase durations.
Durations are wall time and include scheduling, locks, and asynchronous I/O.
Phase finish times use the same origin as imports and GC attempt timestamps.
Setup and final cleanup events are excluded from measured attempt records.
The collector is a single loop, so phase attribution does not rely on task-local
state crossing spawned tasks.

Some phases nest and must not be added together:

- `physical_plan` contains `logical_plan` and `physical_mark`.
- `logical_plan` contains `collector_admission` and `logical_mark`.
- `prune_total` contains admission, validation, commit, and fence release.
- `sweep_payloads` contains blob/chunk claim phases, deletion calls, and
  `finish_deletions`; individual claim attempts nest within claim phases.
- `catalog_commit_attempt` events nest within `catalog_commit`.

Claim attempts minus claim phases count retries in the corresponding sweep
loop. A phase ending does not by itself establish successful reclamation;
interpret it alongside the containing attempt's outcome. Successful-pass
completion remains the benchmark's definition of reported active reclamation.

## Validation

The initial 8-import smoke scenario passed integrity checks and verified that
phase timestamps fit inside their GC attempts. Six focused tests passed:
validation failure/cancellation, publication cancellation, online collection,
and local import/read/GC contention. All-feature/all-target Clippy with warnings
denied passed. The prior full-suite result of 691 passing tests predates this
observability-only change; that full suite was not repeated here.

The repeated runner validates attempt counts, phase timestamps and durations,
pass completion counts, and Busy/retryable-error totals. Each completed scenario
also checks sentinel reads, final collection, leaked pins/claims, fsck, and
byte-verified checkout.

Generated evidence is under
`benchmarks/results/online-holds-phases-2026-09-08/`, including executable hash,
commands, load snapshots, source snapshots, logs, and per-run JSON.

## Results

All three 60-import runs completed and passed integrity and timing-consistency
checks.

| Run | Import seconds | Total GC attempt seconds | Collector admission seconds | Chunk-claim seconds | Prune seconds | Reported removals during / after imports | Final cleanup |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 170.81 | 172.02 | 169.00 | 0.02 | 0.088 | 51 / 0 | 918 |
| 2 | 180.35 | 181.98 | 5.31 | 176.09 | 0.119 | 0 / 34 | 969 |
| 3 | 129.30 | 130.16 | 1.51 | 126.72 | 0.162 | 17 / 17 | 969 |

### Collector admission

Run 1 spent 169.00 of 172.02 seconds of GC attempt time in collector admission
(98.2%). Six catalog-reclamation conflicts preceded five collector-admission
Busy failures. Those failed attempts lasted 11.51, 16.09, 19.32, 27.07, and
43.59 seconds. The final successful admission took another 43.69 seconds.

This phase includes the repository lease, queued pin-release barrier, initial
inventory, and collector acquisition. It does not split each of those costs.
The Busy results identify exhausted collector-admission retries; attributing
every measured second specifically to its CAS would exceed this instrumentation.

The source path is `CollectorLease::try_acquire`: read/refresh a ledger revision,
try `begin_collection`, and retry up to 32 times without adopting a different
collector's recovery authority. Pin updates can repeatedly invalidate admission.

Run 1's successful pass removals plus cleanup total 969 of the expected 1,003
obsolete objects. The remaining 34 are outside successful-pass accounting,
consistent with work completed before a catalog-reclamation error. This profile
does not timestamp individual object deletions.

### Physical deletion claims

The long successful pass in Run 2 lasted 181.98 seconds. Its one chunk-claim
phase lasted 176.09 seconds and made 248 attempts (247 retries).

Run 3's long pass lasted 127.42 seconds. Its chunk-claim phase lasted 126.70
seconds and made 247 attempts (246 retries). There was also one short claim
phase in an earlier pass.

In both cases the claim finished after imports ended. Logical pruning had
already finished at 5.57 seconds in Run 2 and 3.25 seconds in Run 3. Thus these
late pass completions conceal a long interval after logical pruning, spent
trying to acquire physical deletion authority.

`Repository::sweep_page` reads an inventory, filters protected chunks, expands
new manifest pins, then calls `claim_deletions` against that inventory revision.
A rejected claim restarts the loop. It has no attempt limit. The measured
attempt counts directly show repeated rejection in this loop, without the
outer attempt returning Busy.

The deletion method call itself took less than 0.1 ms in each long pass;
`finish_deletions` took less than 0.06 seconds. These are API-phase timings:
packed storage may defer physical work, so they are not a measurement of every
underlying unlink. The claim phase includes inventory reads, protection
expansion, and claim admission; it does not split their individual costs.

### Other phases

Total prune time was 0.088, 0.119, and 0.162 seconds across the three runs.
Catalog-publication totals were below 0.02 seconds per run. The long delays
therefore lie in collector/physical-claim admission in these measurements.
This does not establish a general bound on metadata or catalog work under
other workloads.

## Next changes

1. Combine collector acquisition with its ledger read while checking the exact
   previous collector token. Preserve recovery ownership and cancellation
   cleanup; do not adopt a competing collector or merely increase retries.
2. Close the validation-to-admission race for physical chunk claims.
   Retain manifest expansion, explicit chunk protection, and exact deletion
   ownership. Investigate caching successful immutable manifest expansions
   within a page and admitting claims atomically against validated protection.
   Removing the revision check without an equivalent protection check is unsafe.
3. Rerun these same measurements after each change. Keep catalog-reclamation
   conflicts visible: Run 1 shows they can lead into a different failure pattern.

The phase instrumentation is retained for these follow-ups. No admission or
deletion behavior was changed in this investigation.
