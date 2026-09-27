---
title: Garbage Collection
description: What collection keeps, how to run it, and how it recovers.
---

Casita collects objects that no named root or active operation needs. You can
run collection explicitly. The standard local profile also attempts a
nonblocking pass before a mutation when disk usage reaches 80%. If that pass
leaves disk use at or above 75%, it releases local roots explicitly marked
evictable, oldest used first, and vacuums after each release. Permanent roots
are never selected. A completed pressure pass has a 60-second cooldown shared
by local repository handles.

## What stays live

Each pass marks one logical-state snapshot. It keeps every named root's
forward closure and the data protected by active pins, including reads,
transfers, and unpublished writes. It also keeps the payloads and chunks
needed by those objects. Shared bytes survive while any retained graph uses
them. Unrooted data is allowed and becomes collectible after its pins end.

Collection runs alongside readers and writers; only collectors serialize
with one another. A large traversal spills temporary state to
`<repository>/spill` rather than requiring the entire graph in memory.
Spill files are deleted after the pass, and abandoned files are cleaned up
on the next open. `SpillLimits` bounds temporary memory and disk use.

## Preview and collect

Use the CLI to inspect one collection plan and then run a fresh pass:

```console
$ casita --repository ./cache gc --dry-run
$ casita --repository ./cache gc
```

`--dry-run` computes a plan without changing state. A later `gc` computes a
fresh plan, so counts can change if roots or objects changed meanwhile. Manual
`gc` and `vacuum` do not evict named roots, including evictable ones.

Library callers have three choices:

```rust
# async fn example(repository: &casita::Repository) -> Result<(), casita::Error> {
let preview = repository.preview_collection().await?;
let outcome = repository.collect().await?;     // waits for another collector
let outcome = repository.try_collect().await?; // returns Busy instead of waiting
# Ok(())
# }
```

`collect` waits for another collector. `try_collect` returns `Busy` if
ownership or a racing pin prevents the pass; a scheduler can retry later.
The local pressure check uses nonblocking `try_vacuum` and lets the mutation
continue when collection is busy.

## Shared payload services

Several runners opening the same S3 repository share one root set and one
collector. Its [S3 maintenance guide](../../guides/s3-maintenance/) covers
hold inspection and recovery.

Separate repositories sharing one physical payload service need a different
rule: one repository's roots cannot decide when shared bytes are deleted.
Each repository can prune only its own logical records:

```rust
# async fn example(repository: &casita::experimental::Repository<casita::experimental::MemoryBlobStore, casita::experimental::MemoryMetadataStore>) -> Result<(), casita::experimental::RepositoryError> {
let preview = repository.preview_logical_collection().await?;
let outcome = repository.collect_logical().await?;
# Ok(())
# }
```

Logical collection does not delete payloads or chunks. The shared service
must track ownership across all repositories before reclaiming physical
bytes. See [Shared Payload Services](../shared-payload-services/) for that
contract. `try_collect_logical` gives schedulers nonblocking admission.

## Full-filesystem recovery

Normally, Casita commits the logical prune before deleting physical bytes.
If the standard local profile runs out of space during that commit, it can
delete only payloads already proven stale by the mark, then retry the prune
with the space reclaimed. The pin ledger reserves bookkeeping capacity for
these ownership changes.

This fallback works because local state and payloads share one filesystem.
A custom repository cannot assume that deleting payloads gives its metadata
store space; it returns `StorageFull` instead. Recovery still needs enough
reclaimable garbage to fund the logical commit. If none exists, collection
returns `StorageFull` without deleting rooted data.

## Commit and sweep order

After taking collector ownership, Casita marks roots and pins, verifies
marked records and payloads, atomically prunes unreachable logical records,
then deletes unreferenced payload manifests and chunks. Missing rooted data
aborts before the prune. A physical deletion failure may leave space to
reclaim on a later pass, but does not invalidate rooted data.

The local full-disk fallback above is the one exception to logical-first
ordering. If a process stops after its emergency physical sweep, rooted
graphs remain valid and another `gc` pass finishes cleanup.

After a local collector crashes, reopen the repository and run `gc` to recover
its interrupted claims and prune fence, then audit the recovered state:

```console
$ casita --repository ./cache gc
$ casita --repository ./cache fsck
```

`fsck` may return `Busy` while a collector prevents safe admission. Retry
afterward. Remote recovery uses the exact-token procedure in
[S3 maintenance](../../guides/s3-maintenance/). `fsck` reports reachable
corruption separately from collectible residue; it does not rebuild records
from physical bytes.

## Scheduling and storage ownership

Run collection after imports that leave staging data or at a deployment
specific disk threshold. `DiskPressurePolicy` controls the local threshold
(default 80% used) and calls `try_vacuum`; an external scheduler decides
when to retry `Busy`. Zero reported free bytes still triggers an attempt.

When independent repositories share a physical store, aggregate reachability
or explicit ownership across all of them. `CombinedBlobStore` alone cannot
decide global liveness.
