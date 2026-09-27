---
title: Deduplication
description: How equal blobs and shared chunks reduce storage and transfer work.
---

Casita reuses content at two levels. Equal complete byte sequences share one
`BlobId`. For similar blobs, the standard store uses FastCDC to find chunk
boundaries in the plaintext. Its default target is 256 KiB, with a 128 KiB
minimum and 512 KiB maximum.

A small edit may shift nearby boundaries, but FastCDC usually finds the same
boundaries later in the file. The new blob then stores changed chunks and
reuses unchanged ones. Compression happens after chunking, so it does not
change the blob's identity or those boundaries.

Chunk IDs describe physical storage, not logical objects. A store can change
its chunk size without changing any `BlobId` or `ObjectKey`. Repositories with
different chunk settings can still exchange the same blobs.

During sync, compatible chunk stores can negotiate which chunks the
destination already holds and send only missing chunks. Other transfers stream
the complete plaintext payload. Collection keeps a shared chunk while any
live blob still references it.

See [Blob Storage](../blob-storage/) for reads and physical layout, and
[Sync](../sync/) for transfer choices.
