# GC conflict investigation

The dominant failure in this local Turso-backed benchmark is repository
collector admission against the **file-based pin ledger**. It occurs before
GC reads the Turso metadata snapshot. This run provides no evidence that
Turso database locking caused these Busy attempts.

## Measured reasons

Command:

```sh
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' bench --features experimental --bench online_holds
```

Default object readers; 30 imports × 16 unique 4 KiB files, all four scenarios
completed successfully with final collection, leak checks, fsck, and checkout.

| Failure reason | Imports + GC | Imports + readers + GC |
| --- | ---: | ---: |
| Collector admission raced a pin update (Busy) | 29 | 101 |
| Payload pins changed during collection mark (Busy) | 9 | 12 |
| Catalog pins changed during reclamation (retryable) | 17 | 0 |
| Stale repository revision (retryable) | 10 | 0 |

The combined scenario had 113 Busy attempts: 101 (89%) failed admission,
and the other 12 failed mark validation. It completed no successful GC
passes, either during or after imports. Final cleanup removed all 493
obsolete objects. The earlier run had 103 Busy attempts and one success
after imports; these counts are from separate runs.

Without readers, five passes completed during imports, reporting 34
logical removals. One further pass completed afterward and reported zero.
Final cleanup removed 272 objects; failed passes can make partial progress,
so successful-pass counts undercount total reclamation.

## Why admission fails

`logical_collection_plan_with_recovery` in `src/repository.rs`:

1. Reads the pin inventory and its revision.
2. Calls `CollectorLease::try_acquire` using that revision.
3. Only after successful admission reads the metadata snapshot and begins marking.

`MemoryPinStore::begin_collection`, also used as the transition by persistent
backends, requires both the revision and expected previous collector token
to match. Every reader admission/release and writer protection change
advances the ledger revision. Such a change between steps 1 and 2 rejects
admission even when the set of protections is unchanged.

The local file backend serializes each ledger transaction, but inventory
read and collector acquisition are separate transactions. This is a
repository admission race, not a Turso query or database lock failure.

A deterministic test reproduces the race on memory, file, and object-store
pin ledgers: registering a duplicate pin preserves the protection set but
rejects admission using the earlier revision. Refreshing the revision
allows admission. A competing collector still cannot acquire ownership
without the current collector's exact token.

## Secondary mark conflict

A separate deterministic repository test uses memory metadata and payloads.
After GC marks one garbage object, an idle writer stages new bytes without
publishing any metadata or protecting an existing logical object. The
metadata revision stays unchanged, but GC rejects the marked plan with
`payload pins changed during collection mark`.

A fresh collection with that writer and its staged bytes still alive succeeds,
and the writer can publish its bytes afterward. This isolates a conservative
mark-validation conflict from Turso. It demonstrates one possible cause of
the 12 mark conflicts; the benchmark does not distinguish which pin fields
changed in each of those attempts.

## Recommended next change

First address admission: refresh the inventory and retry acquisition in a
bounded loop when only its revision changed, while preserving exact collector
ownership and recovery checks. No metadata mark exists yet, so this does not
reuse stale reachability results. Rerun the benchmark to measure how many
attempts reach marking and complete during imports.

Then examine whether logical pruning can validate logical protection
separately from physical resource growth. Physical sweeping and catalog
reclamation still need their own protection checks. Do not globally remove
pin revision checks or weaken collector-token checks.

The stale-revision failures without readers reflect the metadata revision
changing before conditional prune commit. They are application-level
optimistic conflicts, and are a separate issue from collector admission.

## Evidence and limits

Generated raw data, executable hash, environment snapshot, and tracked diff:
`benchmarks/results/online-holds-reasons-2026-09-08/`.
Reason counts and during/after splits were checked against totals.
This is one run on a shared workstation; timings are not a controlled
throughput comparison.

The investigation adds benchmark diagnostics and deterministic tests.
It does not change production collector admission or pruning behavior.

Validation: both deterministic tests passed, as did all-feature/all-target
Clippy with warnings denied, formatting, and diff checks.
