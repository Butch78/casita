# Removal of fuser

Removed the vendored crate and patches, both Cargo dependencies and legacy
features, the FUSE-T installer/helper patcher, the adapter, and comparison mount
servers. The native activation logic and its safety/error tests remain.
Historical results and evaluation notes are retained as records, not runnable
legacy backends. Linux continues to use its existing separate FUSE implementation.

The native benchmark corpus now measures native FSKit against the host filesystem.
All native case registrations remain in `benchmarks/manifest.json` and
`benchmark all`, including both sides of the 16/32 reader-cache boundaries.
The obsolete installer-receipt microbenchmark was retired with its implementation.
Memory result schema is v3; repository schema is v2. Host ratios have new names
and cannot be interpreted as historical adapter ratios.

## Validation

- All-feature dependency trees for both crates contain no fuser (`dependencies.json`).
- Nine native activation/protocol tests passed on Linux and the Mac.
- 323 Python benchmark tests passed, with two skips (`python-tests.log`).
- All-feature examples and binaries compile on Linux and macOS (`no-fuser-check.log`).
- Mac primary mount smoke passed execution, byte names, immutable publication,
  busy-close recovery, cleanup, and remount (`no-fuser-smoke.log`).
- Freshly built native repository benchmark: complete with 41 host comparisons,
  first-touch, publication, sandbox, independent-mount, no-TCP, and release gates
  (`no-fuser-repository.json`).
- Native/host launch comparison and fresh-mount first-launch matrix completed,
  including reader-cache pressure checks (`no-fuser-first-launch.json`).
- Migrated transport server mounted the native backend, returned live extension
  read counters, published another root, and unmounted (`no-fuser-transport-smoke.json`).

Mac: ordinary user `hetzner`, Apple M1, macOS 26.6.2. Production smoke reused the
previously signed production bundle; the repository benchmark built and signed
a fresh extension from the current dependency graph.

The full general `filesystem-transports` runner stopped during environment
metadata collection: this archive checkout has no Git repository and `/usr/bin/git`
requires unavailable Xcode tools. See `no-fuser-transports.log`. Its migrated
server was tested separately, without bypassing filesystem correctness checks.
APFS source fixtures use a declared portable filename; native invalid-byte names
remain covered by the dedicated native fixture and production smoke test.

## Reproduce

```sh
cargo test -p casita-fs --no-default-features --features darwin-fskit --lib darwin::
cargo check -p casita-fs --all-features --examples --bins
python3 -m unittest discover -s benchmarks/tests
cargo run --release -p casita-fs --example native_mount_smoke -- /path/to/CasitaFSKit.app
benchmark run native-fskit-repository --prepare-only --output /tmp/prepare.json
# Enable the evaluation extension, then reuse bundle and server_binary from prepare.json:
benchmark run native-fskit-repository --bundle APP --server-binary FIXTURE --repetitions 1 --output /tmp/repository.json
benchmark run native-fskit-first-launch --bundle APP --server-binary FIXTURE --repetitions 1 --output /tmp/launch.json
```

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Run `gzip -dk ./*.gz` in this directory
before running the summary or reproduction scripts. Uncompressed copies are ignored.
