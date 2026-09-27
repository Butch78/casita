---
title: Shared Payload Services
description: Keep physical blobs safe when several repositories share one store.
---

Ordinary collection assumes that one repository owns its payload store. If
several repositories share the same blobs, one repository's roots cannot say
when a blob is safe to delete. Another repository may still have a committed
record for it.

```text
repository A ── records ─┐
repository B ── records ─┼─ BlobId ── shared payload service
repository C ── records ─┘
```

Casita's logical collection removes unreachable records from one repository
and leaves physical deletion to the shared service.

## Choose the right collection API

| Payload ownership | API | What it removes |
| --- | --- | --- |
| One repository owns the store | `preview_collection`, `collect`, `try_collect` | Unreachable records, then unreferenced blobs and chunks. Requires `BlobGc`. |
| Several repositories share the store | `preview_logical_collection`, `collect_logical`, `try_collect_logical` | This repository's unreachable records only. Requires `MetadataStore`. |

The `casita gc` command uses physical collection for a local repository. Do
not add `BlobGc` to a shared backend just to use it: one repository cannot see
all physical owners. `CombinedBlobStore` likewise has no `BlobGc` implementation.

## Use logical collection

```rust
# async fn example(repository: &casita::experimental::Repository<casita::experimental::MemoryBlobStore, casita::experimental::MemoryMetadataStore>) -> Result<(), casita::experimental::RepositoryError> {
let preview = repository.preview_logical_collection().await?;
println!("unreachable records: {}", preview.logical_objects);

let outcome = repository.collect_logical().await?;
if let Some(revision) = outcome.revision {
    println!("pruned at repository revision {revision}");
}
# Ok(())
# }
```

`collect_logical()` waits for active mutation and retention holds;
`try_collect_logical()` returns `Busy` instead. A stale commit fails with a
typed revision error, so retry from a new snapshot. Preview and outcome report
record counts, not bytes reclaimed: one repository cannot know the physical
savings.

An unrooted but committed record still owns its payload. Roots decide which
records survive the next logical collection; the shared service tracks payload
ownership for every committed record, rooted or not.

## Shared service requirements

The service needs an aggregate owner set, reference count, or equivalent
ledger for each physical blob. Its state and ownership changes must share a
durable atomic boundary:

| Event | Required service action |
| --- | --- |
| A new record commits | Add ownership only if the record was inserted; an idempotent retry must not count twice. |
| Logical collection prunes a record | Remove ownership in the same transaction as the record. |
| A root changes | Leave physical ownership alone; roots affect a later prune. |
| An upload finishes before its record commits | Hold a staging lease so deletion cannot race publication. |
| Ownership reaches zero | Delete only after all write leases end; delayed deletion may leak space but must not remove another owner's data. |

The state backend still needs revision checks. The payload service also needs
writer leases and deletion fencing so a stale deletion request cannot remove a
newly published blob. Logical collection alone does not provide these service
rules or resolve concurrent changes to names.

## Multi-tenant confidentiality

Logical collection controls lifetime, not access. A hosted service must
authorize record and payload reads. Exposing a global `has(BlobId)` or raw blob
read would let a tenant test whether another tenant stores known content.
Dedupe hits, physical locations, and cross-tenant storage totals can leak the
same equality signal.

If equality across tenants must remain private, use separate storage or
encryption domains. Removing one tenant's record does not immediately erase a
shared physical blob that another tenant still owns.

## What remains unchanged

`BlobId` still identifies plaintext bytes. Object keys, links, and roots remain
logical state in each repository. Authentication, encryption, aggregate
ownership, and deletion policy belong to the service around Casita.

See [Garbage Collection](../garbage-collection/) for ordinary collection and
[Division of Responsibility](../responsibilities/) for application policy.
