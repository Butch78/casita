# Native crate extraction validation

The memory fixture and repository backend now share the Objective-C callbacks
in the application-independent `fskit-native` crate. These runs use the existing
permanent `native-fskit-portable` and `native-fskit-repository` cases, both already
registered in `benchmarks/manifest.json` and included by `benchmark all`.

## Mounted checks

On the existing macOS test machine, as UID 501:

| Case | Host comparisons | Correctness and cleanup |
| --- | ---: | --- |
| Repository, smoke, one repetition | 41 | Passed |
| Portable memory fixture, smoke, one repetition | 39 | Passed |

The repository suite verified file/range/EOF reads, mmap, nested traversal,
symlinks, script and native execution, mutation rejection, sandbox restrictions,
publication after a cached miss, busy unmount, independent repositories,
teardown, and absence of a TCP listener. The memory suite also verified that
unmounting a second volume leaves the first usable. Neither suite reported a
cleanup error.

The production `native_mount_smoke` example also passed against the extracted
crate's signed development bundle. It verifies the running extension's executable
path, typed publication, raw byte names, execution, mutation rejection, busy-close
retry, cleanup, and remount. The original production bundle registration was
restored afterward. Switching between these development bundle paths initially
failed with ExtensionKit error 2; explicitly registering the new bundle and
restarting the current user's idle FSKit agent allowed the test to pass. Automatic
bundle upgrade handling is not validated by this run.

On macOS, both native crate tests and all seven repository adapter tests passed,
as did the native crate's strict Clippy check. Linux checks also passed: two native
crate tests, seven repository adapter tests, five memory adapter tests, 30 Python
benchmark tests, strict Clippy, formatting, and native crate documentation.

These are smoke validation measurements on a shared machine, not a statistically
controlled before/after performance claim. The memory run uses portable names
because APFS cannot create the invalid UTF-8 source name used by the byte-name
fixture. Native byte-name behavior has separate contract and production mount
checks.

Raw results, binary hashes, source hashes, environment, and executed commands:

- [Repository](repository.json.gz)
- [Memory](memory.json.gz)

Use `gzip -dk ./*.json.gz` to unpack. The recorded binaries precede a
semantically equivalent Clippy cleanup in resource dispatch and the correction
to the Mac unit test's host-side filename fixture.

## Reproduce

Use the Apple SDK/compiler environment appropriate for the host. Run as an
ordinary user. Preparation now creates and activates a signed development bundle through Rust setup. The `bundle` and `server_binary` paths below
come from the preparation JSON.

```sh
python3 -m benchmarks.suites.native_fskit_repository --profile smoke \
  --repetitions 1 --prepare-only --output repo-prepare.json
python3 -m benchmarks.suites.native_fskit_repository --profile smoke \
  --repetitions 1 --bundle APP --server-binary SERVER --output repository.json

python3 -m benchmarks.suites.native_fskit --portable-names --profile smoke \
  --repetitions 1 --prepare-only --output memory-prepare.json
python3 -m benchmarks.suites.native_fskit --portable-names --profile smoke \
  --repetitions 1 --bundle APP --output memory.json
```

Source fingerprints now include `fskit-native`, preventing stale bundle reuse
when its callbacks change.

Production lifecycle check, with the intended bundle registered and no competing
production bundle registration:

```sh
cargo run --release -p casita-fs --no-default-features --features darwin-fskit \
  --example native_mount_smoke -- APP_BUNDLE
```

For the subsequent automatic upgrade fix and five-repetition comparisons, see
[activation and repeated benchmarks](../2026-09-20-fskit-native-upgrades/README.md).
