---
title: Concepts
description: How objects, roots, verification, storage, and collection fit together.
---

Casita stores objects and their links as immutable graphs. A format decides what
an object means and how to verify its identity. A named root points to one object
and keeps its entire reachable graph available.

## Follow an object through the repository

1. An [import](./imports/) reads data and creates verified object records.
2. A [root](./roots-and-retention/) names the graph to keep.
3. Casita stores payload bytes, possibly using chunks and
   [deduplication](./deduplication/). Those storage choices do not change object
   identity.
4. [Sync](./sync/) copies selected objects or roots to another repository. The
   destination [verifies](./verification/) what it receives.
5. [Garbage collection](./garbage-collection/) removes objects that no root or
   active operation still needs.

For example, a Git commit keeps its Git ID, an IPLD block keeps its CID, and a
filesystem directory gets a canonical directory key. All three use the same
root, sync, and collection machinery.

## Two kinds of identity

| Term | What it identifies |
| --- | --- |
| `ObjectKey` | One object in a format's namespace, such as a Git object or Casita directory. |
| `BlobId` | The complete plaintext bytes stored for an object, using a BLAKE3 digest. |

An `ObjectRecord` connects the two: it records the object key, payload ID and
length, and exact forward links. Several objects can share one payload.
Read [repository](./repository/) for the full state model and
[blob storage](./blob-storage/) for physical layout.

Roots and repository revisions belong to one repository. They do not provide
distributed ordering for names copied between repositories.

## Go deeper

- [Directory storage](./directory-storage/) explains canonical filesystem trees.
- [Shared payload services](./shared-payload-services/) covers several
  repositories sharing bytes without sharing deletion authority.
- [Responsibilities](./responsibilities/) shows which decisions applications
  make around Casita.
