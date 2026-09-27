# Native Rust FSKit feasibility — 2026-09-15

## Result

**Rootless native Rust FSKit works on this Mac. Continue the prototype; do not
switch production transports yet.**

macOS 26.6.2, Apple Silicon, UID/effective UID 501. The host and extension are
Rust using objc2, with no Swift or TCP bridge. Ad-hoc development signing,
per-user registration/activation, mounting, simultaneous native volumes and
ordinary unmount/detach all passed. No administrator installation or system
security changes were used. Distribution signing remains unverified.

## Controlled warm-cache comparison

[Standard report](macos-portable-standard.json.gz): five repetitions, 30 samples
per workload, 39 cases per implementation, 585 sample rows and 195 paired
comparisons. Both adapters compile the same immutable Rust backend and portable
fixture. Correctness and teardown passed; `comparison_complete: true`.

Times below are the median of the five per-repetition p50s. Ratios are the median
of paired per-repetition fuser/native ratios, so rounded columns need not divide
exactly. Higher ratios mean lower native latency.

| Workload | Native p50 | fuser p50 | fuser / native |
| --- | ---: | ---: | ---: |
| List 276 entries | 1.108 ms | 7.155 ms | 6.47× |
| Stat 256 files | 1.626 ms | 1.430 ms | 0.88× |
| Open/read/close 4 KiB | 18.7 µs | 593.1 µs | 31.63× |
| Open/read/close 64 KiB | 23.3 µs | 502.0 µs | 21.59× |
| Open/read/close 1 MiB | 81.5 µs | 587.5 µs | 7.19× |
| Held-descriptor read 4 KiB | 0.96 µs | 0.96 µs | 1.00× |
| Held-descriptor read 1 MiB | 62.9 µs | 62.9 µs | 1.00× |
| 32 reads, 1 worker | 0.917 ms | 16.569 ms | 18.19× |
| 32 reads, 4 workers | 1.410 ms | 10.104 ms | 7.14× |
| 32 reads, 16 workers | 1.808 ms | 9.199 ms | 5.09× |

This is preliminary **warm-cache synthetic evidence**, not uncached throughput
or an end-to-end Casita speedup. Correctness warms caches; Python validation is
included in timing. Native FSKit and fuser retain their documented, different
cache/open policies. The near-equal held-descriptor results suggest the large
open/read/close difference warrants investigation of open/close and caching
behavior; callback counters are needed to attribute it precisely.

The machine was shared: other Rust builds were active and load averages were
high ([before](load-before.txt.gz), [after](load-after.txt.gz)). Implementation order
rotates across rounds. Repeat on an otherwise quiet Mac before setting targets.
`decision_eligible` remains false.

## Compatibility finding

[Raw-name report](macos-raw-names.json.gz): native FSKit passed complete byte/name,
metadata, symlink, script execution, mmap, partial read/EOF and two-mount teardown
gates. fuser/FUSE-T changed `byte-\xff` into `byte-�` during enumeration, failing
correctness. Cleanup succeeded. The raw suite remains a failing permanent
regression case; it is not waived by the portable results.

`native-fskit-portable` changes only that name to `byte-ascii` in both adapters,
with a distinct fixture ID and build receipt. Both suites are in the manifest
and `benchmark all`. APFS rejects the raw name, so the raw host control excludes
that one file explicitly; native/fuser raw-name gates retain it.

The raw report's remote filename was `portable-smoke.json`: an initial CLI-wrapper
bug omitted the portable flag. Its configuration, fixture ID, build receipt and
binary hashes correctly record the **raw** fixture. The wrapper was corrected
and tested before the successful portable smoke/standard runs.

## Artifacts and reproduction

- [Portable smoke](macos-portable-smoke.json.gz) and [standard](macos-portable-standard.json.gz).
- [Rootless activation/backup receipt](macos-activation.json.gz), followed by the
  [successful agent restart](macos-activation-restart.json.gz). SIGTERM did not restart
  the agent; the helper now uses the existing Casita setup's guarded SIGKILL.
- [Exact Nix build environment](macos-environment.sh). No active Xcode was present.
  Explicit SDK flags select installed Apple SDK 26.4. Dead-strip-dylibs removes
  unused external libiconv; an explicit FSUnaryFileSystem class reference keeps
  FSKit loaded. No weakened library validation is required.
- `host-control.json` is the earlier Linux-only harness control, not a native result.

From the repository root on this Mac:

```sh
source benchmarks/reports/2026-09-15-native-fskit/macos-environment.sh
python3 -m benchmarks.cli run native-fskit --prepare-only --output /tmp/native-prepare.json
python3 casita-fs/evaluations/native-fskit/enable.py
python3 -m benchmarks.cli run native-fskit --output /tmp/native-raw.json
python3 -m benchmarks.cli run native-fskit-portable --profile standard \
  --repetitions 5 --output /tmp/native-portable.json
```

Run as the ordinary user. The environment file contains this host's Nix paths;
other hosts need equivalent tools. Registered apps and diagnostic reports are
retained under user-owned paths recorded in JSON. Old probe registrations are
removed before a new bundle is selected, preventing stale-code measurements.

Local validation: 31 Python tests; two Rust fixture tests in each name variant;
Rust formatting and diff whitespace checks. Native release builds and mounted
correctness checks ran on the Mac.

## Next

Prefer integrating the same `FilesystemView` and snapshot into both adapters,
proving sandbox repository access and teardown, then adding fresh-mount/callback
counts and repeating on an idle host. Keep fuser as the production fallback.
A narrower alternative is to investigate the byte-name corruption first.

See [prototype design and gates](../../../casita-fs/evaluations/native-fskit/README.md).

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Run `gzip -dk ./*.gz` in this directory
before running the summary or reproduction scripts. Uncompressed copies are ignored.
