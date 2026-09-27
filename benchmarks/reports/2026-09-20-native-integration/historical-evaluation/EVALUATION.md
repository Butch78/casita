# Historical notes before removal of fuser

Commands below describe the former implementation and are not current entrypoints.

# Native Rust FSKit prototype

Requirements: **no root for setup or mounting, preferably Rust throughout our
implementation, and measured speed**. This is an isolated evaluation, not a new
Casita transport or a replacement for `vendor/fuser` yet.

## What this tests

The host executable and FSKit extension are Rust. The extension subclasses
Apple's Objective-C FSKit classes through released `objc2` bindings and uses
Apple's `NSExtensionMain` entry point. There is no Swift, FUSE library,
TCP/Protobuf bridge, or application-owned server process between FSKit and the
fixture. Apple's frameworks and system IPC still exist.

The same `MemoryFilesystem` Rust backend is compiled into the native extension
and the controlled fuser server. It owns the fixture, byte-safe name lookup,
file metadata, and bounded read ranges. Native reads copy directly into FSKit's
supplied buffer; fuser sends borrowed slices. Neither backend reads a repository
or allocates a temporary read buffer. It includes 256 metadata files, arbitrary byte
names, a symlink, an executable shell script, and lengths immediately below,
at, and above 4 KiB, 16 KiB, 64 KiB, 128 KiB and 1 MiB, plus zero and one byte.
These are exploratory boundaries, not claimed performance cliffs.

**Current evidence (2026-09-15):** on macOS 26.6.2/Apple Silicon, the Rust
extension builds, signs ad-hoc, registers, activates and mounts as UID 501.
Native correctness passes, including arbitrary byte names and independent
teardown of two mounts. The fuser/FUSE-T baseline changes `byte-\xff` to
`byte-�`, failing the raw-name gate. A separate portable-name fixture preserves
that failing regression case while allowing controlled performance measurements.
See [recorded results](../../benchmarks/reports/2026-09-15-native-fskit/README.md).

## Reproduce on a Mac

Use macOS 15.4 or later, Apple's SDK/linker (`xcode-select -p`), Cargo and
Python 3. Run everything as the ordinary user. No command uses `sudo`, installs
privileged helpers, writes system application directories, or lowers system
security. A failure requiring any of those is a failed requirement, not a step
to work around.

The test Mac has no active Xcode installation. Its existing Nix Rust 1.96,
Clang 21 and Apple SDK 26.4 work with explicit SDK linker flags; see the retained
[environment](../../benchmarks/reports/2026-09-15-native-fskit/macos-environment.sh).
Dead-strip-dylibs removes an unused Nix libiconv dependency rejected by library
validation; `build.rs` explicitly retains FSKit's dynamically resolved class.
No library-validation entitlement or system security change is needed.

From the repository root:

```sh
python3 -m benchmarks.cli run native-fskit --prepare-only \
  --output /tmp/casita-native-prepare.json
```

The report records the retained app's `bundle` path, binary and source hashes,
signing mode, and command transcripts. The app is built in a fresh user-owned
temporary directory and registered using LaunchServices and PluginKit. It is
retained so its enabled registration can be reused.

Enable **Casita Native FSKit Probe** in System Settings → General → Login Items
& Extensions → File System Extensions. Then use the exact `bundle` path from
the preparation report. The default comparison also requires the existing
FUSE-T runtime. Prepare it with the existing rootless setup command if needed:

```sh
cargo run -p casita-fs --no-default-features --features darwin-fuse --bin casita-fuser-setup
```

For the SSH development host, `python3 casita-fs/evaluations/native-fskit/enable.py`
backs up and updates the current user's enabled-module settings, preserving other
modules, then restarts only that user's FSKit agent. It refuses while any FSKit
volume is mounted. This follows Casita's existing rootless setup procedure.

Alternatively, `CASITA_FUSE_T_LIBRARY` can name an already prepared runtime.
The runner records the selected library path and SHA-256. A missing runtime is
a failed prerequisite, never an automatic switch to another backend.

```sh
python3 -m benchmarks.cli run native-fskit \
  --bundle /tmp/casita-native-fskit-REPLACE/CasitaNativeFSKit.app \
  --profile standard --repetitions 5 --output /tmp/casita-native-measured.json
```

Default signing is ad-hoc **development signing**, following the upstream
example. Its ad-hoc entitlements omit `com.apple.developer.fskit.fsmodule`;
mounting with these entitlements passed on the test host. This is not evidence
of distributable signing/notarization. `--identity NAME` uses the
explicit signing identity and the FSKit entitlement instead. Signed distribution
and any required provisioning remain a separate deployment gate.

The runner creates private sparse raw disk images, attaches them with
`hdiutil -nomount`, and mounts them using `mount -F -t casitanative`. The image
only provides the FSKit resource identity; fixture bytes come from Rust memory.
The process records its nonzero UID, mount success, and ordinary unmount/detach
results. Busy cleanup fails explicitly and never force-unmounts a filesystem.
Retained paths are recorded for diagnosis. Disable/unregister only this probe
when done, using `pluginkit -r` with its exact `.appex` path, before removing its
retained app directory.

## Comparison design

```text
                     identical VFS workload and correctness oracle
                                /              \
                native Rust FSKit           vendored fuser + FUSE-T/FSKit
                       |                              |
                       +---- same MemoryFilesystem ---+
```

There are two distinct comparisons:

| Comparison | Native backend | fuser backend | What it establishes |
| --- | --- | --- | --- |
| Controlled (default) | Shared Rust memory fixture | Same Rust memory fixture | Mounted adapter/transport/cache costs for the same bytes and operations |
| Production reference (`--baseline-server`) | Shared Rust memory fixture | Existing `FilesystemView` and Casita repository | Current end-to-end baseline only; storage is different |

The controlled binary uses our exact vendored fuser patches and the FUSE-T
FSKit backend. Its adapter is deliberately small; it does not reproduce the
production frontend's global lock, repository opens, or publication machinery.
Keep the production reference when assessing the behavior users currently get.
The fixture backend is a benchmark seam, not a new public filesystem API.
The next end-to-end comparison must serve equivalent Casita snapshots through
`FilesystemView` in both adapters, with matched retention and cache settings.

By default, `benchmark run native-fskit` builds the controlled fuser binary,
mounts both adapters, runs the same correctness gates, and pairs every workload,
size, concurrency and repetition. Missing, duplicate, failed, or mismatched
sample sets fail comparison completion. `--comparison native-only` explicitly
collects just the initial native feasibility measurement.

The `comparisons` array contains per-case `p50_fuser_over_native` and
`p95_fuser_over_native`; a value above 1 means lower native latency for that
case. The host control and production reference are never included in these
ratios. There is no aggregate "speedup" across dissimilar operations.
`comparison_complete` also requires successful cleanup. `decision_eligible`
remains false because synthetic memory results do not justify migrating Casita.

Bundle reuse verifies source hashes plus compiler, Cargo, SDK, architecture and
recorded build overrides against the current build environment. The fuser build
must match that environment too. Both binaries use this crate's locked
dependencies and release profile. Binary hashes and the FUSE-T runtime hash are
retained with results.

## Add the production reference

```sh
cargo build --locked --release -p casita-fs --all-features --example transport_server
python3 -m benchmarks.cli run native-fskit \
  --bundle /tmp/casita-native-fskit-REPLACE/CasitaNativeFSKit.app \
  --baseline-server target/release/examples/transport_server \
  --profile standard --repetitions 5 --output /tmp/casita-native-comparison.json
```

Use Cargo's actual executable path if a target-directory override is configured.
All implementations receive the same VFS workloads and expected bytes.
The production transport imports these files into Casita; the native prototype
serves them from memory. Therefore this comparison does **not** isolate transport
cost and cannot establish an end-to-end migration speedup. It is an initial
feasibility measurement. A final comparison must use the same Casita storage
and reader lifetime on both sides.

Measured cases: directory enumeration, 256-file stat traversal, open/read/close
and held-descriptor reads at every size, and concurrent reads with 1/4/16 workers.
Reports retain individual timings and p50/p95. Validation is inside timing;
Python scheduling and validation overhead are included. A correctness pass warms
each filesystem before timing; no cold-cache claim is made. Implementation order
rotates between repetitions, with the actual order retained in the report.

Caching and callback scheduling are system/adapter policies, not artificially
equalized: the fuser fixture uses KEEP_CACHE and a 3600-second entry/attribute
TTL; native FSKit manages its own caching and concurrency. The report records
these differences. Warm results include kernel-cache hits and cannot establish
uncached transport throughput. Fresh-mount/first-touch cases and callback
counters are required before attributing a difference specifically to transport
overhead. They are not yet measured by this suite.

Correctness gates cover full bytes, metadata sizes, directory names including
invalid UTF-8, symlink resolution, script execution, mmap, partial reads and EOF.
Two native mounts must coexist, and unmounting the second must leave the first
usable. This does not yet prove namespace isolation between distinct Casita
repositories, busy-mount policy, native binary execution, or reader release.
Reports always set `decision_eligible: false` while those integration gates remain.

The suite is registered as `native-fskit` in `benchmarks/manifest.json` and
included in `benchmark all`. Non-macOS runs explicitly skip it and leave the
all-suite ledger incomplete. On macOS, a missing extension activation is a
visible failure, never a host-only fallback.

`native-fskit-portable` is also registered and included in `benchmark all`.
It replaces only the invalid-UTF-8 name with `byte-ascii`, in both Rust adapters
and the independent Python oracle. Build receipts and fixture IDs distinguish
the variants; bundles cannot be reused across them. Run it with:

```sh
python3 -m benchmarks.cli run native-fskit-portable --profile standard \
  --repetitions 5 --output /tmp/casita-native-portable.json
```

Passing this suite does not pass the raw-name compatibility gate. APFS cannot
create the raw name either, so the raw suite explicitly excludes that file only
from its host control and optional production-source fixture. The mounted native
and fuser adapters still face the full raw-name gate.

## Local checks

```sh
cargo test --locked --manifest-path casita-fs/evaluations/native-fskit/Cargo.toml --lib
cargo check --locked --manifest-path casita-fs/evaluations/native-fskit/Cargo.toml \
  --target x86_64-apple-darwin --features fuser-baseline
python3 -m unittest benchmarks.tests.test_native_fskit benchmarks.tests.test_all
python3 -m benchmarks.cli run native-fskit --host-only --repetitions 1 \
  --output /tmp/native-host-control.json
```

The cross-check requires the target standard library. It type-checks the actual
macOS callbacks; it cannot validate linking or the Objective-C runtime.

## Real repository integration

The `native-fskit-repository` suite now embeds `Repository::local` and
`FilesystemView` in the Rust extension and compares it with the production
`CasitaFuse` frontend on the **same imported snapshot**. See
[repository design and reproduction](REPOSITORY.md) and the
[measured results](../../benchmarks/reports/2026-09-15-native-fskit-repository/README.md).
The memory-suite limitations above remain specific to those earlier measurements.

## Remaining integration gates

The repository suite validates scoped repository access, staged publication after
cached misses, independent repositories, native execution and orderly reader
release. Cancellation under active I/O, crash recovery, distributable signing,
and a quiet-host application workload remain before switching transports.

## Source and license

Adapted under MIT from [objc2's FSKit example at
0c61cfe589890b087efd83edf85a11b02f55225b](https://github.com/madsmtm/objc2/tree/0c61cfe589890b087efd83edf85a11b02f55225b/examples/fskit).
The original MIT license is retained in [LICENSE-MIT.txt](LICENSE-MIT.txt).
The prototype adapts the example's unreleased macro syntax to published crates,
adds a deterministic read-only filesystem and benchmark runner, and removes the
C logging wrapper. Dependency versions and checksums are in `Cargo.lock`.

See [upstream extension discussion](https://github.com/madsmtm/objc2/issues/815#issuecomment-4042305125)
and [Apple FSKit documentation](https://developer.apple.com/documentation/fskit/).
