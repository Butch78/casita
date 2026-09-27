---
title: Directory Storage
description: How canonical directories link files and subdirectories into a graph.
---

A directory maps validated names to three kinds of entry:

| Entry | Stored information | Retaining link? |
| --- | --- | --- |
| File | Blob ID, byte size, executable bit | Yes, to the blob |
| Subdirectory | Directory ID and recursive entry count | Yes, to the child directory |
| Symlink | Validated target stored inline | No |

The encoding fixes entry order and field representation. Equal directory
contents therefore produce the same `DirectoryId`, which is the BLAKE3 digest
of the canonical payload. Timestamps, ownership, ACLs, and extended attributes
are not part of that identity.

A directory is an ordinary `casita.directory.v1` object. Its format verifier
checks the payload, key, and exact child links. Closure verification also
checks child relationships, including the declared file sizes and recursive
counts. A missing or inconsistent child prevents the directory graph from
being rooted as complete.

Directories and their children form a Merkle graph. A directory's **closure**
is the directory plus every reachable subdirectory and file blob. Roots retain
that closure, sync copies it, and collection keeps it while the root or another
hold is live.

See [Filesystem Imports](../imports/#filesystem-imports) for capture behavior,
[Blob Storage](../blob-storage/) for file bytes, and [Sync](../sync/) for
transfers.
