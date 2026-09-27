# Local durable pin journal

Linux and macOS `FilePinStore` uses a checksummed append journal for staging, explicit
retention, collector/prune ownership, deletion claims, and durable reader-owner
revision reservations. Ordinary process-owned reader pins remain in their
separate volatile inventory. The public pin and metadata APIs do not change.
The object-store conditional-write backend and other platforms’ replacement backend
retain their existing persistence protocols.

## Ordering and acknowledgment

A process-wide registry shares a queue and replay cache across independent handles
for the same canonical ledger path. A blocking worker drains at most 64 requests
already queued; it introduces no batching delay. The existing kernel ledger lock
serializes each batch with every other process and ordinary reader transition.
Operations apply in queue order with the same expected-revision, token, prune,
and deletion checks. A failed conditional check does not invalidate other requests.

Changed durable records become a single sparse delta and one file sync. Every
reply waits for the final durability barrier, including when some operations
checkpoint earlier. An I/O error invalidates tentative cache state before the
kernel lock is released and reports errors to the group. As with existing durable
pins, an unknown result can leave protection recorded; cancellation does not
cancel an in-flight physical update. Lease cancellation cleanup waits for its
registration result before releasing the exact token.

## Format and bounds

`CASPJL01` wraps the existing bounded `CASPIN03`/`CASPIN04` checkpoint codec. Its
4 KiB header carries a random epoch, checkpoint length, checkpoint checksum, and
header checksum. The checkpoint starts at byte 4096; frames start at the next
4 KiB boundary. Each frame has an epoch, body length, operation count, previous
and next revisions, BLAKE3 body/header checksums, and a matching end marker. The
delta contains changed pin/claim records, removed tokens, and scalar coordination
state. Frames occupy disjoint multiples of 4 KiB; appending never rewrites an
acknowledged frame. A zero next-header marks the end.

A group that would exceed 256 journal operations or 1 MiB of frames checkpoints
instead. The encoded inventory remains limited to 64 MiB. Before acknowledging
updates, both active and spare files have allocated space for the resulting
checkpoint, a 1 MiB journal window, and a block of terminator space, rounded up to
a power of two (normally 2 MiB per file initially). Larger inventories require
additional capacity. The cache applies ordinary register, protect, release, and
claim operations directly to current state. Touched records update encoded size
and resource indexes incrementally, while full serialization remains a checkpoint
operation. Reader admission and deletion checks use those indexes; collection and
owner recovery retain conservative full-state validation.

## Checkpoints, migration, and recovery

A checkpoint writes a fresh epoch, complete inventory, and zero terminator into
the spare inode, syncs it, atomically exchanges the active/spare names, and syncs
the parent directory. Rustix maps this exchange to Linux `RENAME_EXCHANGE`
and macOS `RENAME_SWAP`. Unsupported exchange is an explicit error; it is never
emulated with multiple renames or a non-atomic replacement. A failed exchange
leaves the authoritative inventory intact. Both files are grown before acknowledgment; checkpoints
reuse their existing names and extents. On filesystems where overwrites or
metadata updates still need allocation, an OS error remains possible and is
reported; reserved capacity is not a universal guarantee against filesystem
failure. Rust 1.96 implements `File::sync_all` using `fsync` on Linux and
`F_FULLFSYNC` on macOS; sync errors are propagated without a weaker fallback.
Tests on a disposable full volume distinguish this limitation from the
allocation-denial tests that verify reserve reuse without requesting growth.

Initial migration preserves the legacy authoritative state until journal
publication. If growth fails with `StorageFull` before activation, the existing
preallocated replacement path can still update the legacy ledger so collection
can reclaim space. Once activated, no downgrade is attempted. `CASPJL01` fences
older binaries: upgrade every process before allowing a durable mutation. Do not
reset or replace ledger files outside the protocol while repository users exist.

Readers validate the active checkpoint header under the kernel lock. A matching
cached epoch requires replaying only new frames; a new epoch reloads its complete
checkpoint. Recovery syncs the parent directory before adopting a new checkpoint,
and syncs newly replayed frames before exposing their state. This matters when a
writer dies after a complete write but before its sync: a durable reader-revision
reservation must not be used while it is merely visible in the page cache.

A frame with a valid header and missing/mismatched end marker is an incomplete
tail and is ignored. A malformed nonzero header or complete frame with a bad body
checksum/revision fails closed. A torn header may therefore require intervention;
recovery does not discard arbitrary corrupt bytes. Complete unacknowledged frames
may be adopted durably, preserving conservative protection. Tests kill publisher
processes during partial append, before/after sync, before/after checkpoint
exchange, after directory sync, and before reply delivery. They verify that all
acknowledged pins and deletion claims remain, and new operations can proceed.

Permanent `durable-ledger` and `ledger-boundaries` suites measure the replacement
control, contention, and both sides of every production threshold. The unit gates
also exercise cold replay, corruption, cancellation, independent processes,
allocation-denied collection, legacy fallback, unsupported exchange, and mixed
incremental/full-inventory GC groups. Both platforms run the same gates. The
full-inventory collector acquisition, validating prune, and validated deletion
claim paths retain their merged-reader validation and journal their sparse
differences; they do not bypass the group’s final durability barrier. Benchmark commands and timing
scope are documented in [the benchmark corpus](../benchmarks/README.md).

Metadata export/import and backup semantics remain deferred.
