# Atomic logical-prune admission

Moving validation inside the prune fence preserved safety, but separate
inventory capture and fence admission still lost races under reader churn.
The [first fenced-validation measurements](2026-09-08-fenced-validation.md)
record that intermediate result.

Pruning now asks the ledger to check the exact collector and deletion-claim
tokens, install the fence, and return the protected inventory in one operation.
Memory holds one mutex; local files use one locked ledger transaction.
Object-storage ledgers retry the entire operation against their conditional
storage version, recapturing the inventory on a conflict. Custom backends retain
a safe exact-revision retry default.

After admission, every new logical root and snapshot generation is validated
against the immutable mark. The metadata commit still requires the marked
revision. New protection waits during validation and commit; existing holds
retain only their protected data. Physical deletion and emergency recovery
retain their stricter checks. The obsolete checked-root cache is removed.

## Safety coverage

The new backend regression exercises memory, file, and object-storage ledgers:

- Admission includes pins added and grown after the mark, including retired
  history still protected by the collector.
- Wrong collector tokens, incomplete deletion claims, and an existing fence
  reject admission.
- New pin registration and resource growth cannot enter through the fence.
- An incorrect release token cannot unlock it; exact cleanup restores admission.

A gated repository regression exercises validation success and lookup failure,
both with and without caller cancellation. The tracked task keeps the fence
until validation settles, releases it on ordinary failure, and preserves the
expected metadata result. Existing stale-revision, unmarked-root, cancellation,
and emergency-recovery tests remain applicable.

## Benchmark method

Three sequential combined import/read/GC runs, each with 60 imports × 16 unique
4 KiB files and object readers. The existing five-minute scenario timeout
remains. No test or build runs concurrently with these measurements.

Reclamation is counted by successful GC pass completion relative to the end
of imports; individual deletion timestamps are not measured. Final cleanup is
separate. Every completed run checks sentinel reads, final collection, leaked
pins/claims, fsck, and byte-verified checkout.

Generated logs, per-run JSON, source diff, executable hash, commands, and load
snapshots are retained under
`benchmarks/results/online-holds-atomic-2026-09-08/`.

## Validation

The final implementation passed 691 tests with zero failures and 17 ignored
tests across the all-feature suite, including crash matrices, WAL/S3, online
collection, cancellation, Turso multiprocess tests, and doctests.
All-feature/all-target Clippy with warnings denied, formatting, and diff checks
also passed.

## Results

All three runs completed within the existing timeout and passed integrity checks.

| Run | Import seconds | Removed in passes completed during imports | Removed in passes completed after imports | Final cleanup removed | Collector-admission Busy | Reader open p99 (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 159.09 | 17 | 0 | 952 | 7 | 3221.51 |
| 2 | 264.38 | 0 | 34 | 969 | 0 | 4009.06 |
| 3 | 249.33 | 0 | 17 | 986 | 0 | 3483.44 |

No completed run reported a prune-admission Busy failure. This removes the
specific inventory-to-prune-fence contention seen in the earlier experiments,
but the workload still does not show consistent successful reclamation passes
while imports are active.

Run 1 reported an early 17-object pass at 1.59 seconds, seven collector-admission
Busy failures, and one catalog-pin conflict during reclamation. Its successful
pass removals plus final cleanup account for 969 of the 1,003 obsolete objects:
the remaining 34 are outside successful-pass accounting, consistent with work
completed before the reclamation error. Their deletion times are not measured.

Run 2 reported no Busy or retryable failures, but its only completed pass
finished at 265.30 seconds, after imports ended, and reported 34 removals.
Run 3 likewise had no admission or retryable failures, but its only pass
finished after imports at 250.20 seconds and reported 17 removals. These runs
show that eliminating admission failures alone does not ensure useful pass
completions throughout this workload.

Reader admission latency is material, and unrelated builds were active on this
shared host. These timings do not establish a throughput improvement or isolate
the cost of the fence change. Safety checks passed, but the performance work
remains unfinished.

## Next investigation

Measure the duration of collector admission, marking, fenced validation/commit,
physical sweep, and catalog reclamation within each pass. This is needed to
locate the long-running pass seen without admission errors. Collector admission
also retains a separate inventory/revision retry loop; investigate atomic
admission there while preserving exact previous-owner recovery authority.
Keep the existing physical and emergency protection rules.
