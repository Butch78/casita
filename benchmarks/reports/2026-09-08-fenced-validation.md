# Validate logical pins inside the prune fence

The previous six combined-workload runs repeatedly exhausted prune admission:
asynchronous root validation let the ledger revision change before the fence CAS.

Ordinary logical pruning now acquires the fence against an exact inventory
revision, then validates that admitted inventory and conditionally commits
against the marked metadata revision. Admission retries contain no retained-set
or snapshot lookups. The previous checked-root cache is removed because
validation runs once after admission.

The fence covers final validation and commit. Marking and physical sweeping
remain outside it; an existing hold does not block GC for its lifetime.
New payload/logical pins and protection growth wait while the fence is held.
Existing emergency recovery still retains its fence after a failed commit.

## Safety coverage

A gated retained-set regression pauses validation and checks that new admission
is refused. It exercises successful and failed lookups, each with and without
caller cancellation. Cancellation keeps the tracked task's fence until validation
settles. Every ordinary outcome releases the fence, permits new admission,
and leaves the expected metadata and collector state.

Existing regressions cover an absent root followed by an existing unmarked
root, stale publication revisions, and emergency recovery.

## Benchmark method

Three sequential repetitions of the combined import/read/GC scenario:
60 imports × 16 unique 4 KiB files, object readers, and the existing five-minute
per-scenario timeout. Build and tests finish before measurements start.

Results classify reclamation by successful pass completion time relative to the
end of imports, not by individual deletion time. Final cleanup is separate.
Each completed scenario checks sentinel reads, final collection, leaked
pins/claims, fsck, and byte-verified checkout.

Generated evidence is retained under
`benchmarks/results/online-holds-fenced-2026-09-08/`, including executable hash,
commands, source diff, load snapshots, complete logs, and per-run JSON.

## Results before atomic admission

All three runs passed integrity checks, but useful progress was inconsistent.

| Run | Import seconds | Removed during imports | Removed after imports | Final cleanup | Prune-admission Busy |
| --- | ---: | ---: | ---: | ---: | ---: |
| run-1 | 38.43 | 68 | 0 | 935 | 7 |
| run-2 | 67.85 | 0 | 0 | 1003 | 7 |
| run-3 | 71.40 | 0 | 0 | 1003 | 8 |

The remaining race is the separate inventory read and fence CAS, even without
root lookups between them. The next change combines those into one ledger edit
that checks collector ownership and returns the admitted inventory.

Before this follow-up, all 690 tests passed, 17 were ignored, and all-feature/
all-target Clippy, formatting, and diff checks passed.
