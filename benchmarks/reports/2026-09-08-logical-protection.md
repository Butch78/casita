# Separate logical and physical pin validation

Ordinary logical pruning now compares only the inputs to logical reachability:
the maximum pinned snapshot generation and the union of closure roots and
explicit Object resources. This mirrors `mark_pin_scopes`. Changes to physical
catalogs, blobs, chunks, storage paths, or metadata paths alone no longer
invalidate a logical mark.

The logical prune still acquires its fence against the current ledger revision,
checks collector ownership, and conditionally commits against the marked
metadata revision. It does not ignore newly protected logical roots or higher
snapshot generations.

Physical sweeping continues reading current protections and conditionally
claiming deletions. Catalog reclamation and emergency pre-prune deletion
retain the complete payload-protection comparison. The weaker logical
comparison is used only in the ordinary prune admission path, not for
authorizing physical deletion.

## Safety evidence

A deterministic regression marks an unrooted blob as garbage, then stages the
same bytes in a writer without publishing metadata or pinning a logical object.
Before this change, execution failed with the payload-pin-change Busy error.
Afterward, GC removes the old logical record but preserves the physical blob;
the still-active writer publishes it successfully and fsck remains healthy.

A pin-ledger test runs on memory, file, and object-store backends. Physical
resource growth and catalog changes preserve logical protection, while Object
resources and higher snapshot generations change it. Equivalent closure and
Object pins compare equally, as do redundant older snapshot holds.

Existing negative repository tests still reject new logical object/snapshot
protection after marking. The broader selected library suite passed:
197 tests, with 11 pre-existing ignored tests, covering repository, pin-ledger,
packed-storage, emergency collection, and recovery.

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
| Logical-pin mark Busy failures | 0 | 14 |
| Catalog-pin retryable failures | 22 | 0 |
| Stale metadata revision failures | 0 | 0 |
| Successful passes during imports | 10 | 0 |
| Reported removals during imports | 51 | 0 |
| Successful passes after imports | 1 | 1 |
| Reported removals after imports | 0 | 493 |
| Final cleanup removals | 85 | 0 |

**The combined workload still did not report reclamation in passes completed
during imports.** Its single successful pass finished after imports and
reported all 493 obsolete objects removed. This change removes a demonstrated
physical-only false conflict, but does not establish sustained reclamation
with concurrent readers and imports.

Without readers, successful passes reported 51 removals during imports.
Final cleanup removed 85; the remaining obsolete objects were not accounted
for by successful reports because retryable catalog failures can follow
partial progress.

The remaining Busy diagnostic explicitly identifies logical protection
changes. It does not identify which root or generation changed, or establish
whether that newly protected data was already reachable from the mark.
A useful next investigation is to distinguish new roots already covered by
the mark from roots that actually extend reachability. Newly protected
unmarked objects must remain safe.

This is one shared-workstation run, not a controlled throughput comparison.
The raw log, parsed measurements, binary hash, environment snapshot and
tracked diff are under
`benchmarks/results/online-holds-logical-2026-09-08/`.
Reason and during/after counts were checked against every scenario's totals.

Final validation: all 14 application, S3, online-GC, contention, and
cancellation integration tests passed. All-feature/all-target Clippy with
warnings denied, formatting, and diff checks passed.

## Follow-up review

Reviewed the entire uncommitted implementation, including scoped-reader
lifetimes, pin-before-snapshot validation, collector admission and cancellation,
logical versus physical mark comparisons, sweep claims, and benchmark accounting.
No production correctness blocker was found. This does not establish a
general liveness guarantee: the combined benchmark's reclamation limitation
above remains.

The full `cargo test --all-features` run passed 686 tests across library, CLI,
integration, and documentation suites, with zero failures and 17 ignored tests.
This includes WAL/S3, Turso multiprocess, and process-crash matrix tests.

The physical-reuse regression was then strengthened and rerun for plain blobs,
chunked payloads, and a late manifest-only pin. It verifies that chunks survive
even without explicit per-chunk protection, reads and compares all bytes, and
publishes the staged object after collection. The extended test passed.

All-feature/all-target Clippy with warnings denied, the default build check,
documentation with warnings denied, formatting, and diff checks passed.
Only test coverage and this review record changed during the review.
