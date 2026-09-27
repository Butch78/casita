# fskit-native

An unpublished crate for reusable native Rust FSKit integration. It implements
the FSKit filesystem, volume, and item callbacks in Rust, together with
security-scoped path resource ownership and retryable backend shutdown.

It has no dependency on Casita. On macOS it uses `objc2`, `objc2-foundation`,
`objc2-fs-kit`, and `block2`. The portable contract uses the standard library.
Consumers implement `Filesystem` and optionally `Control`; `BackendSession`
coordinates concurrent operations and shutdown.
Names and symlink targets are bytes, reads use caller-provided buffers, and
directory snapshots retain optional attributes across paginated enumeration.

The extension executable calls `native::register` from its constructor, supplying
an `Extension` with a display name, filesystem type, accepted resource kinds, and
a backend factory. Its Info.plist must use `FSKitNativeFileSystem` as
`EXExtensionPrincipalClass`. The executable links Apple's extension entry point.
Both path resources and block-device-backed fixtures are supported. File reads
can borrow immutable data or return owned bytes without an extra temporary copy.

The current adapter supports immutable files, directories, symlinks, an empty
xattr namespace (or FSKit emulation), and optional backend-defined creation
callbacks. It is not a general writable filesystem implementation. Creation can
serve application control operations, but ordinary writes and other mutations
return `EROFS`. Volume statistics currently use fixed synthetic values.

The optional `setup` feature provides rootless registration and activation:
`setup::register(app, "Example.appex", "org.example.filesystem")`. It verifies
the signature and module identity, serializes settings changes across modules,
and upgrades bundle paths only when no FSKit volumes are mounted. Registration
changes force an agent restart even for an already enabled module; interrupted
activation can be retried. Use a new app path for upgrades. Automatic activation
requires access to macOS's protected FSKit settings. If macOS denies that access,
setup completes registration and any required agent restart successfully.
Inaccessible settings do not mean that approval is missing. Attempt the mount;
macOS checks approval, and the caller should provide enablement instructions if
it fails. Other settings errors still stop setup before registration changes.

Normal mounts should call `setup::installed("Example.appex", "org.example.filesystem")`
to discover and verify the selected bundle without changing registration
or restarting the agent. Keep the returned `Installation` alive until unmounting.
Its shared lock permits independent mounts while preventing explicit setup from
replacing their installation. `app()` identifies the installed bundle and
`property()` reads signed extension metadata for application protocol checks.
Pending activation returns an error requiring explicit setup.
Discovery does not read macOS's protected activation settings. macOS checks user
approval when mounting; enable the extension in System Settings under General →
Login Items & Extensions → By Category → File System Extensions.

The consumer owns its app identity, signing, packaging, and OS mount/unmount
orchestration. No Swift or additional IPC bridge is required.

The native adapter derives from the objc2 FSKit example. Its MIT attribution is
preserved in `LICENSE-MIT.txt`.

Casita's memory fixture and repository implementation exercise the same contract:

```sh
cargo test --locked --manifest-path crates/casita-fskit/Cargo.toml --features repository --lib
```

See [contract design and limits](CONTRACT.md).
