# Native FSKit integration validation

Test host: `hetzner@23.88.76.133`, Apple M1, macOS 26.6.2, ordinary UID 501.
The production bundle uses `org.casita.fskit.extension` and filesystem name
`casita`. This is a correctness/lifecycle check, not a new performance benchmark.
The existing permanent native/fuser benchmark corpus remains available.

## Reproduce

```sh
python3 casita-fs/native-fskit/package.py --output /absolute/path/CasitaFSKit.app
cargo run --release -p casita-fs --no-default-features --features darwin-fuse \
  --example native_mount_smoke -- /absolute/path/CasitaFSKit.app
cargo test -p casita-fs --no-default-features --features darwin-fuse \
  --lib darwin:: -- --skip benchmark
python3 -m unittest benchmarks.tests.test_native_fskit benchmarks.tests.test_native_fskit_repository
```

## Results

The public library API passed repository-backed mount, file reads, executable
launch, directory/file/symlink publication, non-UTF-8 names, immutable content,
exclusive repository ownership, busy-unmount rejection and retry, control-file
cleanup, and remount. See `native-integration-smoke.log`. Packaging signature
verification and binary hashes are in `native-integration-package.log`.
Darwin-module unit tests: 20 passed on both Linux and macOS. Benchmark harness
tests: 12 passed. See `linux-tests.log` and `native-integration-tests.log`.

Re-registering an existing app reproduced extensionKit error 2 on subsequent
mount. Setup now queries PluginKit and reuses a matching registration, rejecting
conflicting bundle paths. The test host's stale agent state from the failing
implementation was cleared once, with no FSKit volumes mounted; the successful
smoke test then mounted, unmounted and remounted without an agent restart.

The package was ad-hoc signed. This validates local integration, not Developer
ID distribution/notarization or unattended activation on the restricted fresh
Mac. One controller per repository and explicit recovery after crashes remain
API constraints. Arbitrary in-process readers continue through the older API.

## Default backend migration

Native FSKit is now the primary `darwin::PersistentMount` and the default
macOS feature (`darwin-fskit`). Its constructor accepts repository, mount-parent,
and app-bundle paths. Publication accepts `casita::Node`. The older transport
is exposed as `FuserBenchmarkMount` only with `fuser-benchmark` (or the historical
`darwin-fuse` feature alias). `casita-fs-setup` now registers the native bundle;
`casita-fuser-setup` retains comparison-runtime provisioning.

`default-dependencies.json` records that normal library defaults and the
extension's `production` feature both exclude fuser. Linux native-only unit
tests passed: 19, with one performance benchmark intentionally ignored.
All-feature examples/binaries compiled on Linux, and 12 harness tests passed.

```sh
cargo test -p casita-fs --no-default-features --features darwin-fskit --lib darwin::
cargo check -p casita-fs --all-features --examples --bins
cargo run --release -p casita-fs --no-default-features --features darwin-fskit \
  --example native_mount_smoke -- /absolute/path/CasitaFSKit.app
```

Mac validation after the default switch passed: the native-only primary API
completed the full lifecycle smoke test (`native-default-smoke.log`), the
production extension compiled without fuser (`native-default-extension-check.log`),
and all-feature examples and setup binaries compiled (`native-default-check.log`).
The smoke test reused the already signed v1 extension bundle; this change
changes API selection, features, and setup entry points, not the mount protocol.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Run `gzip -dk ./*.gz` in this directory
before running the summary or reproduction scripts. Uncompressed copies are ignored.
