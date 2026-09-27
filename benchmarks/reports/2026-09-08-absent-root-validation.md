# Absent roots and validation retries

Ordinary logical pruning now permits newly protected roots absent from its
marked metadata snapshot. It still checks every new root: an existing unmarked
object rejects pruning even if an absent root appears first in the same pin.
Higher snapshot generations still require a new mark.

The conditional metadata commit retains the exact marked revision. If a writer
publishes a previously absent object before pruning commits, the revision
check rejects the stale prune. Staged physical bytes remain protected by the
existing physical pins and deletion fences. Emergency pre-prune deletion
continues using the stricter complete protection check.

## Regression coverage

Two regressions failed before the exception and pass afterward:

- A new unpublished object root permits the original collection plan to remove
  unrelated garbage. Publishing first instead produces StaleRevision; its bytes
  survive and a fresh collection succeeds.
- A pin containing an absent root followed by an existing root only permits
  collection if that existing root is already marked. Both explicit Object
  resources and closure-scope roots are exercised.

The complete all-feature suite after this exception passed 689 tests,
with zero failures and 17 ignored tests, including WAL/S3, Turso multiprocess,
process-crash matrices, and doctests. Clippy passed.

## Remaining race and cached validation

The first three 60-import runs no longer reported logical-root conflicts,
but exhausted prune-fence admission retries. Validation performs retained-set
and snapshot lookups before attempting admission against the earlier ledger
revision. Readers and writers can advance that revision during the lookups.

A subsequent change caches successful root checks only within one prune
admission attempt, whose marked snapshot and retained set remain fixed.
Retries avoid repeated I/O for already-checked roots; every newly observed
root and snapshot generation still gets validated. The cache is discarded
with the attempt.

A regression verifies that repeated validation of an absent root performs one
retained-set lookup, then checks and rejects a newly added existing unmarked
root despite the cache. After this change, 201 selected repository, pin-ledger,
and packed-storage library tests passed, with 11 pre-existing ignored tests.
The full 689-test result above predates this cache-only change.

## Six benchmark runs

Each run used only the combined import/read/GC scenario:
60 imports × 16 unique 4 KiB files, object readers, and the existing
five-minute per-scenario timeout. All six executions passed sentinel reads,
final collection, leaked-pin/claim checks, fsck, and byte-verified checkout.

```sh
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' bench   --features experimental --bench online_holds --no-run
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 CASITA_BENCH_READER_SCOPE=object CASITA_BENCH_SCENARIO=imports_readers_gc   /tmp/casita-online-holds-build/release/deps/online_holds-6a75bc045fd4d5b4
# Three repetitions for each implementation.
```

| Validation | Run | Import seconds | Removed in passes completed during imports | Prune-fence Busy failures | Other retryable failures | Final cleanup removed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| absent | 1 | 115.7 | 0 | 7 | 0 | 1003 |
| absent | 2 | 111.1 | 0 | 6 | 1 | 1003 |
| absent | 3 | 87.6 | 0 | 6 | 1 | 1003 |
| cached | 1 | 44.54 | 0 | 7 | 2 | 1003 |
| cached | 2 | 73.64 | 0 | 7 | 2 | 1003 |
| cached | 3 | 88.81 | 0 | 8 | 1 | 1003 |

**None of the six runs completed a pass reporting reclamation during imports.**
All 1,003 obsolete logical objects remained for final cleanup in each run.
Any successful in-workload passes reported zero removals. The remaining Busy
reason was consistently prune-fence admission contention.

Caching avoids repeated lookups as proven by the regression, but did not
resolve this workload's progress failure. Timing varies on this shared machine;
these results do not support a throughput claim.

## Next step

Close the race between logical-pin validation and prune-fence acquisition.
One candidate is to acquire a bounded, short-lived fence before validating
the admitted pin inventory, then release it on every validation error,
stale-revision failure, or cancellation. Another is atomic ledger admission
against a validated logical-protection set.

Either approach requires tests for new protection arriving at admission,
validation I/O failures, cancellation, and exact collector ownership. It must
avoid turning lifetime holds into a global GC barrier or leaving an admission
fence behind after a rejected plan. Merely increasing retry counts does not
address the gap between validation and acquisition.

## Evidence

Generated raw logs, per-run JSON, aggregate measurements, exact executable
hashes and commands, load snapshots, and tracked diffs are retained under:

- `benchmarks/results/online-holds-absent-2026-09-08/`
- `benchmarks/results/online-holds-cached-2026-09-08/`

Counts and successful-pass timestamps were validated for all runs. The
first directory contains the full-suite test log; the second contains the
post-cache library-test log.

Final post-cache validation: seven online-GC, contention, cancellation, and S3
integration tests passed. All-feature/all-target Clippy with warnings denied,
formatting, and diff checks passed.
