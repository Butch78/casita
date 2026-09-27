---
title: Blob Storage
description: How Casita identifies, stores, and reads immutable payload bytes.
---

A **blob** is a byte sequence identified by the BLAKE3 digest of its complete
plaintext, its `BlobId`. Equal bytes have the same ID even if different
importers wrote them or stores chunked and compressed them differently.

`BlobStore` supplies presence checks, readers, streaming writes, and optional
chunk information. The standard native store uses FastCDC chunks compressed
with zstd. A manifest binds those chunks into one blob; a single-chunk blob
can omit the manifest because its chunk ID already equals the blob ID.

## Read guarantees

The standard chunked store checks a chunk's digest before returning its bytes.
An unseeked sequential read also checks the complete `BlobId` at EOF. A caller
that stops early has not completed that whole-blob check. Use
`Repository::open_verified` when bytes must be authenticated before they are
returned. It reads a Bao proof and fails if that proof is unavailable or
invalid. Bao also supports checking a selected range without reading the rest
of the blob. See [Verification](../verification/) for the distinction.

Roots and collection belong to `Repository`, not `BlobStore`. The payload
backend alone cannot decide when a blob is safe to delete.

## Put a near tier in front of a far tier

With the `experimental` feature, `CombinedBlobStore::new(near, far)` reads from
the writable near store first and falls back to the far store when a blob is
absent. Writes go only to near, and reads from far do not fill near.

```rust
use casita::experimental::{CombinedBlobStore, MemoryBlobStore, MemoryMetadataStore, Repository};

fn tiered_repository() -> Result<
    Repository<CombinedBlobStore<MemoryBlobStore, MemoryBlobStore>, MemoryMetadataStore>,
    Box<dyn std::error::Error>,
> {
    let near = MemoryBlobStore::new();
    let far = MemoryBlobStore::new();
    let payloads = CombinedBlobStore::new(near, far);
    Ok(Repository::new(payloads, MemoryMetadataStore::new()?))
}
```

The adapter has no `BlobGc` implementation because the repository cannot know
who else still needs data in a shared far store. It also has no combined
`BlobSync`, so transfer uses whole payload streaming. A repository using a
shared payload service can prune its own records with `collect_logical`, while
the service controls physical deletion. See
[Shared Payload Services](../shared-payload-services/) for that contract.

### Repair a near tier from a replica

`RepairingBlobStore::new(near, far)` accepts two `ChunkedBlobStore` values. It
checks the complete near blob before returning an ordinary reader. If near has
typed corruption or a referenced chunk is missing, it verifies the complete
far blob, repairs near, and checks the replacement. Permission and backend
errors do not trigger repair. A failed repair retains both the near error and
the repair error in `BlobRepairError`.

`verified_read` can also rebuild missing or corrupt Bao data from verified near
bytes. The adapter collects only near data; the far replica needs its own
retention policy. Its extra full read makes it suitable when verified repair
matters more than read latency.

Read [Deduplication](../deduplication/) for chunk reuse and the
[Experimental Rust API](../../reference/experimental-rust-api/#payload-storage)
for backend methods.
