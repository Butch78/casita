# Atomic collector and physical-claim admission

The [phase investigation](2026-09-08-gc-phase-timing.md) found two admission
bottlenecks: collector acquisition consumed 169 seconds in one run, while a
single chunk page required 248 and 247 admission attempts in two others.

## Implementation

Collector acquisition now checks the exact previous-owner token and installs
new ownership in one ledger operation, before marking. Memory uses one mutex;
file ledgers use one exclusive transaction; object-storage ledgers retry the
whole operation against their conditional storage version. The tracked lease
task still owns acquisition and cleanup through caller cancellation. Custom
backends retain an exact-revision retry default.

Ordinary post-prune blob/chunk claims now check current protection atomically:

- Collector ownership must match, and no logical-prune fence may be active.
- Direct pin resources and overlapping deletion claims must be disjoint from
  the candidate batch, including retired pin history.
- A chunk claim requires every currently pinned manifest identity to be among
  those checked against that candidate page. A newly pinned unchecked manifest
  rejects admission and causes the sweep to refresh and expand it.
- The operation accepts only blob/chunk resource claims. Catalog and physical
  storage-path reclamation retain their existing protocols.

Within a shrinking page, successful immutable manifest expansions are cached:
chunks excluded by an expansion are never reintroduced into the page. Absent
manifests are checked again on retry. The original mark already accounts for
its pinned manifests. Emergency pre-prune sweeping continues to use the strict
marked revision and complete protection checks.

The new operation does not hold a global fence while resolving manifests.
The ledger transaction checks current protection immediately before recording
the deletion claim. Unrelated direct resources and duplicate reader ownership
can change without invalidating that check.

## Regression coverage

Across memory, file, and object-storage ledgers, tests cover:

- Collector acquisition after incidental pin churn; competing ownership and
  takeover require the exact previous token, and takeover preserves history
  and outstanding claims.
- An unchecked late manifest rejects a chunk claim; listing checked manifests
  never overrides direct chunk protection or retired history.
- Metadata-only churn and disjoint chunk growth allow a validated claim even
  when an older exact-revision claim fails.
- Logical fences, overlapping claims, and incorrect collector tokens reject
  admission; admitted claims block conflicting registration and pin growth.
- Non-payload claim types are rejected by this API.

Repository regressions additionally exercise late manifest-only pins, reuse of
payloads selected by an earlier mark, publication cancellation, and emergency
recovery.

## Measurement

Three sequential repetitions of the same combined import/read/GC workload:
60 imports × 16 unique 4 KiB files, object readers, four runtime workers, and
the existing five-minute scenario timeout. Phase timing stays enabled.
Tests and builds finish before measurements start. The shared machine means
elapsed times are diagnostic, not a controlled throughput comparison.

Generated evidence is retained under
`benchmarks/results/online-holds-admission-2026-09-09/`, including executable
hash, commands, source snapshots, test/build logs, load snapshots, and per-run
JSON. Every completed run checks final collection, pin/claim cleanup, sentinel
reads, fsck, and byte-verified checkout. Attempt and phase accounting are
validated. Reported active reclamation still counts successful passes completed
during imports; it does not timestamp individual deletions or partial failed
passes.

## Validation

The full all-feature suite passed 693 tests with zero failures and 17 ignored,
including crash matrices, WAL/S3, cancellation, and multiprocess tests.
All-feature/all-target Clippy with warnings denied, formatting, and diff checks
passed.

## Results

All three runs completed within the unchanged timeout and passed integrity
and phase-accounting checks. No run reported a Busy admission failure.
Across nine chunk pages there were nine claim attempts: zero page retries.
Previously the two long pages needed 248 and 247 attempts.

| Run | Import seconds | Total chunk-claim milliseconds | Chunk pages / attempts | Catalog-reclamation conflicts | Reported removals during imports | Final cleanup removed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 137.16 | 44.98 | 3 / 3 | 78 | 17 | 952 |
| 2 | 137.78 | 44.14 | 3 / 3 | 79 | 17 | 952 |
| 3 | 186.12 | 48.40 | 3 / 3 | 80 | 17 | 952 |

The longest individual GC attempts were 6.86, 4.28, and 6.13 seconds. This
workload no longer exhibits the measured multi-minute chunk-claim retry loop,
but it does not yet show sustained successful reclamation during imports.

Each run reported 17 removals in successful passes during imports and zero
in successful passes afterward. Successful-pass totals plus final cleanup
account for 969 of 1,003 obsolete objects. The other 34 are outside successful-
pass accounting, consistent with work done before reclamation errors. Their
individual deletion timestamps are not recorded.

Reader-open p99 was 2.48, 1.58, and 2.58 seconds. These remain substantial waits;
shared-machine timing and a remaining failure path prevent a general throughput
claim.

## Remaining catalog-cleanup conflict

Every retryable error was `catalog pins changed during reclamation`, from
`finish_payload_collection`. That phase can reclaim retired and unpublished
representations after logical pruning, the primary sweep, and catalog publication.
It validates a separate payload/catalog pin mark before physical-path deletion.

Because these errors abort collection completion, subsequent attempts can
retain released pin history and outstanding recovery claims. Collector
acquisition is now atomic, but repeated attempts still pay for ledger reads,
writes, release barriers, and marking. For example, Run 1 spent 78.24 seconds
across 84 collector-admission phases despite having no admission failures.
The admission timer includes more than the atomic ledger edit itself.

Next, review whether a changed-pin catalog-cleanup attempt can be deferred
while safely completing the already-settled logical/physical collection.
Tests must distinguish a known pre-deletion conflict from uncertain or still-
running deletion, preserve protected catalog paths, and prove exact claim and
collector cleanup. Keep the stricter catalog protection checks; merely accepting
a stale catalog mark would be unsafe.

Changes remain uncommitted.
