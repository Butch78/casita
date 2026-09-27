# Diagnose new logical roots absent from the mark

All 25 Busy attempts in the instrumented combined run reported the same
first conflict: a newly pinned object key was absent from GC's marked metadata
snapshot, and the current metadata revision still matched that snapshot when
checked. This identifies an avoidable rejection candidate involving staged,
unpublished keys, rather than an existing logical object selected for deletion.

No GC admission or deletion rule was relaxed in this investigation.

## Source of the pins

The generic publication path in `src/repository.rs` builds its staging overlay,
then protects every staged record's object key, its links, and root-set targets
before committing metadata. Those keys need not exist in the metadata snapshot
GC previously marked.

Current logical pin validation accepts new roots only if they are already in
the retained set. An unpublished key cannot be in that set, so it causes Busy.
A fresh mark can handle the same absent key: reachability traversal tolerates
unpublished inputs already present in the mark's pin inventory.

## Diagnostics and deterministic reproduction

Logical pin validation now returns the first conflict rather than only a
boolean. Prune admission retains the actual marked snapshot and distinguishes:

- A higher snapshot generation.
- A new root that exists in the marked snapshot but is unmarked.
- A root absent from the mark, with metadata revision unchanged.
- A root absent from the mark, with metadata revision advanced.
- Collector ownership or prune-fence changes.

The snapshot's revision remains the conditional commit revision. Existing
reject/accept decisions remain unchanged; extra reads classify rejected marks.
If a diagnostic lookup fails, the error propagates without pruning.

A deterministic memory-backed test marks old garbage, stages a new object,
and pins that new object key. It checks both before-publication and
after-publication cases. Both reject the original mark with the expected
distinct reason. The strengthened test also verifies that a fresh collection
safely removes the old garbage while the writer stays alive, preserves the new
bytes, and permits the unpublished object to be published afterward.

The diagnostic returns the **first** uncovered root in key order. An inventory
may contain other conflicts too. This does not prove that every new root in
each rejected inventory was harmless.

## Instrumented benchmark

```sh
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_SCENARIO=imports_readers_gc   cargo --config 'build.build-dir="/tmp/casita-online-holds-build"'   bench --features experimental --bench online_holds
```

The new optional scenario filter permits focused investigation; unset retains
the original four-scenario benchmark. This run used 60 imports, 16 unique
4 KiB files per import, object readers, and the existing five-minute timeout.

| Metric | Result |
| --- | ---: |
| Import duration | 16.45 seconds |
| Busy: root absent from mark, metadata unchanged | 25 |
| Other Busy reasons | 0 |
| Other retryable errors | 0 |
| Successful passes completed during imports | 3 |
| Objects reported removed in those passes | 408 |
| Objects reported removed after imports | 595 |
| Final cleanup removals | 0 |

Two early passes removed zero objects; a pass completed at 5.481 seconds
reported 408 removals. The final pass completed at 17.885 seconds and reported
595 removals, after the 16.445-second import interval.

Integrity, byte verification, and pin/claim leak checks passed. This positive
progress result does not reverse the preceding three longer runs' lack of
useful pass completions during imports. Diagnostics add reads and alter timing;
machine load also varied sharply. Treat this run as conflict-classification
evidence, not a GC progress fix or throughput comparison.

## Proposed next change

For each newly protected root outside the retained set:

1. Check whether it exists in the marked metadata snapshot.
2. Reject the mark if it exists and is unmarked.
3. Otherwise permit that absent root, while validating every other new root.

Keep higher-generation snapshot checks, collector ownership, prune admission
at the inspected ledger revision, and conditional metadata commit unchanged.
If the staged object is published first, the metadata revision check must
reject the stale prune. Physical resource pins and deletion claims must
continue protecting staged bytes throughout this sequence.

This change should be validated with mixed pins (an absent staged root plus
an existing unmarked dependency), concurrent publication, and the longer
repeated benchmark. Simply ignoring an entire pin containing an absent root
would be unsafe.

## Evidence and validation

The selected repository and pin-ledger suite passed 114 tests. Seven online-GC,
contention, cancellation, and S3 integration tests passed. The final strengthened
regression and all-target/all-feature Clippy are recorded separately.

Raw output, parsed measurement, exact command, executable hash, environment
snapshot, and tracked diff are under
`benchmarks/results/online-holds-root-diagnostics-2026-09-08/`.
