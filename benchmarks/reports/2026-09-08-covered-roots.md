# Accept new protection already covered by the logical mark

Logical prune admission now accepts new closure roots or explicit Object
resources if each additional root is already present in the exact retained
object set. Roots protected by the original pin inventory remain covered by
the original mark. A higher maximum snapshot generation still requires a new
mark; snapshot coverage is not inferred from checking a few roots.

The retained mark is closed over object dependencies. The conditional metadata
commit still validates the marked revision, so accepting an already-marked
root cannot silently accept changes to its graph. Membership checks run
before acquiring the prune fence against the checked ledger revision; a
concurrent pin update forces revalidation. Collector ownership, cancellation
handling, physical sweep claims, and emergency full-protection checks remain.

Membership uses the existing RetainedObjects interface, including indexed
lookups for spilled marks. Lookup errors propagate and prevent pruning.

## Deterministic tests

The new repository regression failed before the change with
`logical pins changed during collection mark`.

It creates a named live root and unrelated unrooted garbage, marks them,
then protects either the already-marked root or the unmarked garbage. It
exercises both closure-scope pins and explicit Object resources. Protection
of the marked root now permits collection of the garbage and leaves the root
readable; protection of the unmarked object still rejects collection and
preserves its bytes.

A separate test confirms that new snapshot generation zero differs from no
snapshot protection, and a higher generation still rejects a stale mark.
Redundant older snapshot protection does not enlarge the marked generation.

The selected repository, pin-ledger, and packed-storage suite passed:
199 tests, with 11 pre-existing ignored tests. This includes the strengthened
late manifest-only pin regression and emergency recovery paths.

## Benchmark

Command:

```sh
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' bench --features experimental --bench online_holds
```

Default object readers, 30 imports × 16 unique 4 KiB files per scenario.
All four scenarios passed final collection, leaked-pin/claim checks, fsck,
and byte-verified checkout.

| Metric | Imports + GC | Imports + readers + GC |
| --- | ---: | ---: |
| Admission Busy failures | 0 | 0 |
| Logical-pin mark Busy failures | 0 | 9 |
| Catalog-pin retryable failures | 22 | 0 |
| Stale metadata revision failures | 2 | 4 |
| Successful passes during imports | 8 | 2 |
| Reported removals during imports | 34 | 289 |
| Successful passes after imports | 1 | 1 |
| Reported removals after imports | 289 | 170 |
| Final cleanup removals | 17 | 34 |

**The combined workload reported 289 obsolete objects reclaimed in two
passes completed while imports and readers were active.** A subsequent
in-flight pass completed after imports and reported 170 removals; final
cleanup removed the remaining 34. Together these account for all 493
obsolete logical objects in this run.

This demonstrates progress under the tested schedule. It does not guarantee
continuous progress under arbitrary contention. Nine logical-pin conflicts
and four stale metadata revisions remain. The preceding run reported no
successful reclamation during combined imports, but these shared-machine
runs are not a controlled throughput comparison.

Successful-pass counts can still undercount deletion when a retryable pass
makes partial progress, as seen without readers: 34 + 289 + 17 is less than
493. Completion timestamps describe passes, not individual deletion times.

Raw output, parsed measurements, executable hash, environment snapshot, and
tracked diff are in
`benchmarks/results/online-holds-covered-2026-09-08/`.
Reason totals and during/after splits were checked for each scenario.

Final validation: all 14 application, S3, online-GC, contention, and
cancellation integration tests passed. All-feature/all-target Clippy with
warnings denied, formatting, and diff checks passed.
