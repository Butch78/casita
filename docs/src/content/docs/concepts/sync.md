---
title: Sync
description: How Casita copies verified objects and roots between repositories.
---

Sync copies selected objects or named roots from one repository to another.
`casita sync` uses the library's `transfer()` engine. The source holds a stable
revision while the destination verifies what it receives.

## What gets copied

An object selection copies its forward closure by default. A root selection
also copies its full closure, then installs the same name at the destination.
For a filesystem root, a path selection copies only the chosen file or
directory closure. Casita checks the ancestor directories to establish the
path, but does not publish them at the destination.

The receiver skips identical records, rejects conflicting immutable records,
and verifies payloads through each object's format before publication. Peers
with compatible chunk stores can avoid sending chunks already present; other
payloads use plaintext streaming. Object records publish in bounded batches.
Requested roots move only after their complete closures verify, so a failed
transfer can leave useful objects behind without moving a root to an incomplete
graph. Retrying is safe. Sync does not delete destination roots or objects.

## Complete discovery or faster repeats

By default, Casita examines the full selected source closure, including below
objects already present at the destination. `--incremental` stops below an
identical object if its destination closure is already verified. This can make
repeat transfers faster, but it does not check source descendants below that
reused object. A missing or incomplete destination closure is still discovered
and verified normally.

In Rust, `TransferOptions::default()` uses complete discovery. Use
`with_discovery(TransferDiscovery::ReuseVerified)` for the incremental policy.
Discovery queues and visited sets can spill to temporary disk when they exceed
the memory threshold. The spill-byte limit is enforced before roots move.

## Source options

Local and S3 repositories can supply source data; the optional SSH adapter
reads a remote repository through OpenSSH. An SSH source holds one revision and
uses OpenSSH for authentication and host-key checks. It sends whole plaintext
payloads over one ordered stream; remote chunk negotiation is not available.

With `--from-blobs`, `--from` supplies the revision, roots, and object records,
while a second retained Casita repository supplies payloads. Their revisions
may differ because the destination verifies each payload against the selected
records. If the payload source lacks required bytes, the transfer fails rather
than falling back to `--from`.

See [Synchronize Repositories](../../guides/sync/) for commands and endpoint
requirements.
