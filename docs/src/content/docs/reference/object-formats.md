---
title: Object Formats
description: Built-in namespaces, native IDs, and the links each format retains.
---

An `ObjectKey` names a logical object using a format namespace and native ID.
Its `ObjectRecord` also stores a `BlobId`, the BLAKE3 digest of the complete
plaintext payload, plus direct links. The two IDs can differ: a Git object's
key uses its Git OID, while its payload has a BLAKE3 `BlobId`.

A format verifier checks the key against the payload, enforces the format's
encoding rules, and reproduces the record's exact sorted, unique links. Those
links decide what a root retains, what sync copies, and what collection keeps.

## Built-in registry

`FormatRegistry::builtin()` contains the formats below. The supported
`Repository::memory` and `Repository::local` constructors, and the generic
`Repository::new`, use it automatically. To choose an exact set of verifiers,
use `FormatRegistry::new(...)` with
`casita::experimental::Repository::with_formats`. The registry is immutable,
and each namespace can have only one verifier. See
[Define a Custom Object Format](../../guides/custom-formats/) for a working
example. Registry composition is part of the `experimental` API.

## Filesystem formats

| Namespace | Native ID | Direct links |
| --- | --- | --- |
| `casita.blob.v1` | BLAKE3 digest of the plaintext bytes | None |
| `casita.directory.v1` | BLAKE3 digest of the canonical directory payload | Child file blobs and directories; symlinks are inline |

A canonical directory preserves names, file sizes, executable bits, recursive
directory counts, and symlink targets. It omits timestamps, ownership, ACLs,
and extended attributes.

## Application metadata

| Namespace | Native ID | Direct links |
| --- | --- | --- |
| `casita.linked.v1` | BLAKE3 digest of the canonical linked payload | Its sorted, unique object keys |

`casita.linked.v1` stores a set of links and an opaque application body. The
built-in verifier checks the framing and links, not the body's application
meaning. `LinkedObjectFormat::new(namespace)` can use that same framing under
an application-owned namespace when explicitly registered.

## IPLD formats

| Namespace | Native ID | Direct links |
| --- | --- | --- |
| `ipld.raw.v1` | Canonical CIDv1 with raw codec `0x55` and BLAKE3-256 multihash | None |
| `ipld.linked.v1` | Canonical CIDv1 with Casita codec `0x300001` and BLAKE3-256 multihash | Its sorted, unique CID set |

`ipld.linked.v1` also uses a fixed Casita payload format. It does not accept
arbitrary IPLD codecs. Both IPLD namespaces require CIDv1 and a 32-byte
BLAKE3 multihash.

## Native Git formats

Native Git objects store their exact Git body. Their key uses the SHA-1 or
SHA-256 OID of Git's `<type> <size>\0<body>` framing. SHA-1 verification uses
collision detection.

| Namespace | Native ID | Direct links |
| --- | --- | --- |
| `git.sha1.blob.v1`, `git.sha256.blob.v1` | Git blob OID | None |
| `git.sha1.tree.v1`, `git.sha256.tree.v1` | Git tree OID | Blobs, symlink blobs, and subtrees; gitlinks are not generic links |
| `git.sha1.commit.v1`, `git.sha256.commit.v1` | Git commit OID | Tree and parent commits |
| `git.sha1.tag.v1`, `git.sha256.tag.v1` | Annotated tag OID | Exact typed target |
| `git.view.v1` | BLAKE3 digest of the canonical view payload | Direct ref targets and optional exact native pack cache |

A Git view fixes one hash format and an immutable set of direct or symbolic
refs. It may name a default ref. Symbolic refs resolve within the view and do
not add separate links. The payload also records the exact reachable-object
inventory; ordinary object links still retain those objects. A CLI view named
`origin` is selected by the root `git/origin`.

## Unknown or unavailable formats

Stored records still carry links, so listing and retention can use them without
decoding the payload again. A source can traverse those links for transfer, but
a destination needs the format verifier to accept the object. New publication
and exact closure validation also require it. Existing roots continue to
retain the graph. `fsck` reports an unavailable verifier as `Unchecked`, not
as corruption by itself.
