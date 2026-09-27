---
title: Verification
description: What Casita checks during publication, reads, sync, and integrity scans.
---

Casita checks stored bytes at different boundaries. The guarantee depends on
which operation you use:

| Operation | What is checked |
| --- | --- |
| Import or sync | The destination's registered format verifier checks each object's native ID, canonical payload, and exact direct links before publishing its record. |
| Root publication | The complete reachable graph and format relations must verify before the root moves. |
| Chunked read | Each fetched chunk is checked against its `ChunkId` before its bytes are returned. |
| Sequential read from the standard chunked store | An unseeked, complete read checks the `BlobId` at EOF. Stopping early leaves that check unfinished. |
| `open_verified` or `cat --verified` | A Bao proof authenticates bytes against the `BlobId` before each byte is returned; successful EOF also confirms the length. |

A seek or partial read cannot complete that sequential check. Use a verified
read when bytes must be authenticated before use. Bao
proofs also allow a selected range to be checked against the blob ID without
reading the rest of the payload. If the required proof is unavailable, a
verified read fails rather than returning unchecked bytes.

`fsck --audit-only` checks the repository's current snapshot for payload,
format, relation, and closure problems without running the repair pass. An
unavailable format verifier is reported as `Unchecked`: the data remains
retained, but its format validity is not established. See
[Errors and Integrity](../../reference/errors/) for findings and
[Object Formats](../../reference/object-formats/) for the registered rules.

A hash proves that bytes match a known digest. It does not establish who
supplied that digest or who may access the data. Applications need a trusted
source for expected root IDs and authorization around services that expose
repository content.
