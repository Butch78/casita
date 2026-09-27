# Native repository FSKit integration

The [reusable filesystem contract](../fskit-native/CONTRACT.md) is implemented by both the memory
fixture and repository backend. Both use the native callbacks in `fskit-native`.

The host and extension are Rust using Apple FSKit through `objc2`. Repository
reads run inside the extension, without a TCP bridge. Requires macOS 26+, an
ordinary user account, and an Apple SDK when building.

## Build and test

From the repository root, with Cargo and Python 3 available:

```sh
python3 crates/casita-fskit/package.py --output /absolute/path/CasitaFSKit.app
cargo run -p casita-fs --bin casita-fs-setup -- /absolute/path/CasitaFSKit.app
cargo test --release -p casita-fs --no-default-features --features darwin-fskit \
  --test native_mount native_mount_lifecycle -- --ignored --nocapture
cargo test --release -p casita-fs --no-default-features --features darwin-fskit \
  --test native_mount concurrent::independent_processes -- --ignored --nocapture
```

The output directory must be new. The default signing identity is ad-hoc for
local development. `--identity` selects a signing identity; this script does
not provision certificates, profiles, or notarize the bundle. Distribution
signing and fresh-machine installation remain separate release gates.

The ignored mount tests reuse the installed extension and may run alongside other
FSKit mounts. The concurrent test mounts two repositories in separate processes,
closes and remounts one, and checks that the other remains readable without
changing registration, module settings, or the existing agent. Setup and upgrade
validation is a separate opt-in test requiring exclusive FSKit access; see the
[validation commands](../casita-fs/README.md#validation).

On 2026-09-20, both commands passed on the macOS arm64 validation host using the
ad-hoc signed `CasitaFSKitConcurrent.app`. The lifecycle test covered execution,
publication, busy-close handling, cleanup, and remount. The concurrent test
covered separate processes and repositories, fresh-file reads after closing and
remounting a sibling, and unchanged registration, enablement, and agent identity.
No unrelated FSKit volume was mounted during these runs.

On 2026-09-21, the file-layout cleanup passed all 13 native library tests,
production packaging/signature checks, fixture binary builds, and both mount
tests on the same macOS arm64 host with Rust 1.96 and Apple SDK 26.5. The build
now strips unused dylib dependencies from the app binaries: without this flag,
the Nix toolchain's unused libiconv caused a library-validation launch failure.
FSKit remains explicitly linked. The original installed bundle was restored
after validation, with no FSKit volumes left mounted.

## Library API

`casita_fs::darwin::PersistentMount` accepts a local repository directory
and a parent directory for the mount. No app path is needed after explicit setup.
`publish_root` accepts a byte name and a `casita::Node`, returning its path under `views/`.
Directory, regular-file, executable-file, and symlink roots are supported.
Publication is immutable. Keep imported objects durably reachable and flush
repository writes before publication. Run synchronous mount methods outside
an async executor.

Normal mounts discover the user's selected extension and verify its
signature and `CasitaMountProtocol` metadata. Independent Casita processes and
checkouts reuse this shared installation for different repositories. Shared
installation locks allow concurrent mounts and exclude setup until they close.
Missing, incompatible, or pending activation requires explicit setup; mounting
does not register bundles or restart the user's agent.

Setup verifies the app signature, registers the extension, and enables the
module where permitted through `fskit-native`'s optional `setup` feature.
Existing matching registrations are reused. After all FSKit volumes are unmounted,
setup replaces a different registered bundle path and restarts the user's FSKit
agent. Interrupted activation leaves a retry marker; rerun setup to finish.
Package upgrades into a new app path instead of overwriting a registered bundle.
The benchmark runners use the same Rust setup automatically; no Python activation
script is required. macOS privacy policy can deny access to the
activation settings, particularly through a restricted SSH daemon. Signing
does not remove that access requirement. Setup still succeeds after registration
and any required agent restart: inaccessible settings do not prove approval is
missing. macOS checks approval during the actual mount, and mount failures retain
instructions for enabling Casita through System Settings.

See [headless activation and upgrade options](../../benchmarks/reports/2026-09-21-fskit-headless/README.md) for tested rootless
workflows, the shared-agent restriction, and alternatives under investigation.

Only one native mount controller per repository is currently supported.
`close()` performs ordinary unmount; busy mounts return an error and can be
retried. On unsuccessful cleanup, control files and the mount directory are
retained. A crash leaves `.casita-native-active` and its referenced session.
Before explicit recovery, verify that its volume is unmounted and no controller
or extension is still using it; never remove control files from a live mount.

This is the default macOS mount API. `NativeRepositoryMount` remains an alias.
The fuser dependency, adapters, and setup runtime have been removed.
Benchmarks compare native FSKit with ordinary host reads.

The permanent benchmark corpus builds from `crates/casita-fskit`. See the [benchmark guide](BENCHMARKS.md).

## File layout

- `src/repository.rs` and `src/repository/`: repository backend, caches, and diagnostics.
- `src/repository_adapter.rs`: implementation of the reusable filesystem contract.
- `src/memory.rs` and `src/contract_tests.rs`: memory fixture and shared contract checks.
- `extension/`, `host/`, and `build.rs`: app bundle entry points, metadata, and entitlements.
- `fixtures/`: repository import/cache-pressure utility and executable test fixture.
- `setup/`: development activation helper used by the benchmark runners.
- `package.py`: production bundle packaging and signing.

Native callbacks and activation machinery live in `../fskit-native`.
This crate remains a separate Cargo workspace with its own locked dependencies.
