# casita-fs

Byte-safe, read-only filesystem views over Casita repositories.

## Platform backends

- Linux: `linux-fuse` uses `fuse-backend-rs` with `StoreFs` and `FuseMount`.
- macOS: `darwin-fskit` uses our native Rust FSKit extension with
  `darwin::PersistentMount`. Repository reads run inside the extension, without
  fuser, FUSE-T, or a TCP bridge.
- Both platform features are enabled by default. `--no-default-features`
  provides filesystem views without a mount transport.


`FilesystemView` still accepts arbitrary `ContentReader` implementations for
in-process traversal and Linux mounts. Native macOS mounts require a local
repository that the extension can open independently.

## macOS setup and migration

Requires macOS 26+, an ordinary user account, and a packaged native app:

```sh
python3 crates/casita-fskit/package.py --output /absolute/path/CasitaFSKit.app
cargo run -p casita-fs --bin casita-fs-setup -- /absolute/path/CasitaFSKit.app
cargo test --release -p casita-fs --test native_mount native_mount_lifecycle -- --ignored --nocapture
cargo test --release -p casita-fs --test native_mount concurrent::independent_processes -- --ignored --nocapture
```

Setup is an explicit, once-per-user operation. It verifies the bundle signature,
registers the extension, and enables the current user's module. Run setup while
FSKit volumes are idle. Normal mounting discovers that shared installation and
checks its signed mount-protocol version without changing registration, changing
enablement, or restarting the user's FSKit agent. Missing, incompatible, or
incompletely activated installations return an actionable setup error.
Automatic activation requires access to macOS's protected settings. If access is
denied, setup registers the app and returns manual-enablement instructions.
Normal discovery does not read those settings, allowing an already registered
and approved extension to mount over SSH. macOS enforces approval at mount time.
Enable Casita in System Settings → General → Login Items & Extensions → By Category
→ File System Extensions, then retry the mount. The By App toggle can fail even
when the By Category toggle works. A registered bundle alone does not authorize
a mount; mount failures retain the macOS diagnostic and include these directions.
Ad-hoc signing works for local development; distribution signing and fresh-Mac
activation remain release gates.

The primary API changes from `PersistentMount::new(parent)` to:

```rust,ignore
let mut mount = casita_fs::darwin::PersistentMount::new(
    repository_path, mount_parent,
)?;
let path = mount.publish_root(b"my-root", node)?;
mount.close()?;
```

Publication accepts a `casita::Node`, replacing the old `FilesystemView`
argument. Flush repository writes and keep the objects durably reachable for
the mount lifetime. Run synchronous lifecycle methods outside an async executor.
`close()` returns an error for busy mounts and permits retry. Independent Casita
processes can mount different repositories concurrently, including from different
checkouts, using the same installed extension. Mounts hold shared installation
locks; explicit setup refuses replacement while they remain open. One native
mount per repository is currently supported. `NativeRepositoryMount` remains
an alias for the same implementation.

See [native packaging and recovery instructions](../casita-fskit/README.md),
[validation results](../../benchmarks/reports/2026-09-20-native-integration/README.md).

## Validation

```sh
cargo test -p casita-fs --no-default-features --features darwin-fskit --lib darwin::
cargo check -p casita-fs --all-features --all-targets
```

The macOS mount tests are ignored by default because they mount real filesystems.
`native_mount_lifecycle` checks publication, execution, busy unmount, and remount.
`concurrent::independent_processes` uses two child processes and repositories,
closes and remounts one while checking the other, and verifies that registration,
enablement, the agent, and pre-existing FSKit mounts stay unchanged. These tests
reuse the installed extension and can run alongside other FSKit volumes.

Registration and replacement are tested separately and require exclusive FSKit
access. Both bundles must declare the current mount protocol. This opt-in test
leaves the replacement bundle registered:

```sh
CASITA_FSKIT_APP_BUNDLE=/absolute/path/CasitaFSKit.app \
CASITA_FSKIT_UPGRADE_APP_BUNDLE=/absolute/path/CasitaFSKitUpgrade.app \
  cargo test --release -p casita-fs --test native_mount native_setup_upgrade -- --ignored --exact --nocapture
```
