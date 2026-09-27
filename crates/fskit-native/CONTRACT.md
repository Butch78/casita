# Reusable filesystem contract

The `fskit-native` workspace crate implements the native callbacks and defines
the backend boundary. Its portable API has no Casita, Apple, or async-runtime
types. Both the memory fixture and repository backend implement it and use the
same native callback adapter. Apple bindings are macOS-only dependencies.

## Operations

- Inodes are opaque `u64` identifiers. Each backend supplies its root identifier.
- Names and symlink targets are uninterpreted bytes. Metadata preserves file kind,
  size, parent, and executable permission bits.
- `read` takes a caller-owned buffer and leaves its unused tail unchanged. Methods
  may run concurrently and must run outside an async executor.
- `read_data` optionally returns borrowed or owned initialized bytes for the
  native callback. Memory reads borrow their content; repository reads preserve
  the existing allocating reader path. FSKit buffers are never exposed as Rust
  slices of uninitialized bytes.
- Enumeration returns an immutable snapshot with optional entry attributes.
  Rejected entries do not advance the cookie. The transport must retain the
  snapshot and bind its cookies to a verifier for the duration of enumeration;
  new publications appear in new snapshots. The transport synthesizes dot entries
  and retains up to 128 enumeration snapshots. Expired or mismatched verifiers
  return `ESTALE`, requiring a restart instead of silently skipping entries.
- `Control` is separate from ordinary reads. Casita's `PublishRoot` validates a
  staged descriptor through the existing repository implementation. Publication
  policy and descriptor formats do not belong in the reusable filesystem layer.
  The native adapter delegates creation to `Filesystem::create`; Casita maps
  that callback onto its `Control` implementation, restricted to `views`.
- Optional observations retain callback timing and xattr metrics. Diagnostic
  filenames, reserved inode IDs, JSON serialization, and release reports belong
  to the Casita adapter, not the native crate.

## Lifecycle

`BackendSession::with` admits concurrent operations while retaining a shared
lock. `close` requires exclusive admission and returns `WouldBlock` while an
operation is active. It calls the backend's shutdown barrier, retaining the
backend if that fails. After success it drops the backend and rejects new work.

This is backend lifecycle only. Successful OS unmount must be handled separately,
and a transport must retain the session on busy or uncertain unmount. Dropping a
session does not imply that the volume was unmounted or the barrier succeeded.
Backends must not let independently owned resources or background operations
escape admission without accounting for them in shutdown.

## Deliberate limits

The repository adapter preserves its allocating read implementation and caches
the metadata snapshot for immutable directories. Publication directories are
refreshed. This extraction does not claim a performance improvement; validation
uses the existing permanent native memory and repository benchmark suites.

Security-scoped resources, errno mapping, native registration, timestamp options,
and xattr policy now live in `fskit-native`. Its optional `setup` feature also owns
rootless registration, activation, and idle bundle upgrades.
App identity, packaging, and OS mount orchestration remain with the consumer. Volume statistics are still
synthetic; richer metadata and writable filesystem support are outside this API.

## Verification

From the repository root:

```sh
cargo test --locked --manifest-path crates/casita-fskit/Cargo.toml --lib
cargo test --locked --manifest-path crates/casita-fskit/Cargo.toml --features repository --lib
```

The contract tests exercise both implementations, byte names and symlinks,
executable metadata, short reads and EOF, pagination backpressure, publication
snapshot isolation, concurrent admission, failed shutdown retry, and reopening
the repository after release.
