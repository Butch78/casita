# Shared Rust activation and repeated FSKit comparisons

## Implementation

`fskit-native` now owns reusable Objective-C callbacks, byte-safe filesystem
contracts, resource ownership, shutdown admission, and optional rootless setup.
Casita implements the repository policy through an adapter. The crate has no
Casita dependency.

Production and both benchmark variants use the same Rust setup implementation.
The Python activation script has been removed. Setup verifies signatures and
module identity, serializes shared settings updates, refuses registration changes
while any FSKit volume is mounted, and replaces an idle bundle registration.
A persistent marker preserves a required agent restart across failures.

The initial version restarted the agent but returned before asynchronous FSKit
discovery settled. A first mount failed with a helper-communication error; retrying
the same bundle worked. The final mount paths retry only the observed helper-communication/ExtensionKit
startup errors for a 15-second retry window, and only if the target remains
unmounted. Ambiguous command timeouts and other failures are not retried.
Individual mount commands remain separately bounded.

We also tested [Apple's FSClient discovery API](https://developer.apple.com/documentation/fskit/fsclient).
In this SSH session it listed only Apple's three built-in modules, omitting the
registered benchmark extension. It therefore cannot serve as a required readiness
gate here. The final implementation uses the actual mount outcome. See [the original failure](discovery-failure.json.gz).

Use a new app path for upgrades rather than overwriting an installed bundle.
The permanent `native_mount_smoke` example accepts a second bundle path, tests
that upgrades are rejected while mounted, and checks the actual extension
executable path before and after the upgrade.

## Measured results and limits

On the shared M1 macOS 26.6.2 machine as UID 501, all five ordinary-operation
rounds finished with 205 paired comparisons (41 cases per round). Their
correctness checks and repository release barrier passed. Three complete
executable-workload rounds also finished, covering shared and distinct executables
at 1, 8, 15, 16, 17, 31, 32, and 33 workers. These existing permanent cases cover
both sides of the reader-cache bounds and remain in `benchmarks/manifest.json`
and `benchmark all`.

The fourth workload round failed when a **host-filesystem** `awk` process exceeded
120 seconds. Another job had started a FUSE-T mount on the same machine. Later,
a newly launched Rust test binary also stalled before reaching its test harness;
a one-second process sample showed only `_dyld_start`. These observations do not
establish that our FSKit implementation caused the timeout. They also do not
justify claiming a completed five-round launch run. The failed run remains
`complete=false`; no raw success flags have been changed. Cleanup reported no
errors and our benchmark mounts were removed.

[Comparison tables](COMPARISON.md) report all five complete ordinary-operation
rounds and only the first three whole launch rounds, discarding the incomplete
fourth launch round in full. They include median and range, and the analysis
revalidates each retained matrix using the permanent benchmark correctness gates.
These are shared-host diagnostic results, not an isolated regression comparison,
and no cold OS-cache claim is made. The results predate the final mount startup retry, which executes outside timed filesystem operations.

Selected medians:

| Operation | Host | Native FSKit |
| --- | ---: | ---: |
| Warm 4 KiB open/read/close | 19 µs | 19 µs |
| Warm 1 MiB open/read/close | 74 µs | 73 µs |
| Stat 256 files | 1.62 ms | 1.70 ms |
| Directory listing | 0.17 ms | 0.98 ms |
| 17 shared executables, first batch | 251 ms | 376 ms |
| 17 shared executables, immediate repeat | 93 ms | 97 ms |
| 17 distinct executables, first batch | 2,479 ms | 3,585 ms |
| 17 distinct executables, immediate repeat | 100 ms | 100 ms |

The ordinary cases have five repetitions; executable batches have three.

## Validation

- Linux: 16 native crate tests, 11 filesystem tests, and seven repository adapter
  tests passed. All 335 Python benchmark tests passed, with two platform skips.
- Strict Clippy passed for the root crate, `casita-fs`, and `fskit-native`.
  Formatting, diff checks, and strict filesystem/native crate docs passed.
- macOS: 17 native crate tests passed, including a real temporary-plist test.
  Both shared setup and the filesystem crate passed strict Clippy on macOS;
  the filesystem check includes all features and targets.
- The prior extraction's production mount, execution, byte names, publication,
  busy-close, cleanup and remount passed; see the
  [extraction report](../2026-09-20-fskit-native-extraction/README.md).
- Final automatic-upgrade mounted validation and a complete repeat benchmark
  remain pending an idle Mac. The shared machine's other live FSKit mount must
  remain untouched. The alternate Mac's SSH process cannot read protected FSKit
  settings (`EPERM`), so it was not used as a substitute.
- The previous PR's macOS CI failed to find `FSKit.framework` in its default SDK.
  `devenv.nix` now selects `apple-sdk_26` on Darwin. CI must validate that environment
  change. The existing Linux verified-CLI test also failed on a missing outboard
  file; that storage test is outside this activation change.

## Reproduce

Use an Apple SDK with FSKit and Rust 1.96.0. Run as an ordinary user authorized
to access FSKit settings, with no other mounted FSKit volumes before setup.
The runners now register and activate their own signed development bundles.

```sh
export CASITA_WORKLOAD_AWK=/nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk
export CASITA_WORKLOAD_SORT=/nix/store/26h13pzgcz97dc7x11nbvd2h6g0j6dk5-coreutils-full-9.11/bin/coreutils
export CASITA_WORKLOAD_GZIP=/nix/store/3fdibjln064bpvcgnn4lb5fla4rfr6ip-gzip-1.14/bin/.gzip-wrapped
python3 -m benchmarks.suites.native_fskit_repository \
  --profile standard --repetitions 5 --workloads --timeout-seconds 1800 \
  --output repository.json
python3 -m benchmarks.suites.native_fskit \
  --portable-names --profile standard --repetitions 5 --output memory.json
cargo run --release -p casita-fs --no-default-features --features darwin-fskit \
  --example native_mount_smoke -- APP_A APP_B
```

Use equivalent relocatable binaries on another machine. Tool hashes, exact
commands, source fingerprints, build environment, timing samples and cleanup
results are retained in [the incomplete raw run](shared-host-timeout.json.gz).
The final mount command requires two signed bundles with the production module
ID and distinct paths. Restore the original test-machine bundle through Rust
setup afterward.

Regenerate the retained subset tables from the repository root:

```sh
python3 benchmarks/reports/2026-09-20-fskit-native-upgrades/summarize.py --partial
```
