# Native warm-launch timestamp fix, 2026-09-18

## Decision

Use immutable timestamps of one second after the Unix epoch, matching the
production fuser frontend. The prototype previously reported zero for all item
timestamps. Changing these timestamps eliminates repeated parent-directory
enumeration during warm process launches in this fixture.

This is now the native repository prototype's default. A `zero-timestamps`
marker retains the old behavior for same-binary regression comparisons.
Production fuser code was not changed in this investigation.

Final default results, median of three round p50s in milliseconds:

| Metadata siblings | Native script | fuser script | Host script | Native binary | fuser binary | Host binary |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 3.947 | 4.936 | 3.860 | 2.635 | 6.711 | 2.563 |
| 128 | 3.476 | 4.463 | 3.440 | 2.376 | 6.410 | 2.284 |
| 256 | 3.460 | 4.437 | 3.416 | 2.392 | 6.393 | 2.264 |
| 512 | 3.479 | 4.550 | 3.574 | 2.600 | 6.601 | 2.519 |

These are warm launches on a shared M1 Mac running macOS 26.6.2 as UID 501.
The final fixed sweep has zero native enumeration callbacks for both direct
script and native binary launches. The former density-dependent penalty is
absent. Absolute timings remain subject to shared-host variation.

## Causal control

The initial 512-sibling comparison changed only native item timestamps through
a runtime marker. Store timestamps ran first, zero timestamps second, using
identical source/build/binary and snapshot receipts. Both passed all gates.

| Native operation | Zero timestamps | Store timestamps | Enumeration callbacks, zero → store |
| --- | ---: | ---: | ---: |
| Direct script launch | 28.745 ms | 3.488 ms | 12 → 0 |
| Native binary launch | 13.491 ms | 2.285 ms | 6 → 0 |
| Security code-object creation | 2.939 ms | 2.966 ms | 6 → 6 |

The VFS reports native atime, mtime and ctime as exactly 0 or 1,000,000,000 ns
according to the selected mode. Fuser reports 1,000,000,000 ns in both modes.
The implementation also changes birth and added times from zero to one second.
This experiment does not isolate which individual timestamp enables reuse or
identify the particular macOS cache involved. It does establish that the
timestamp difference controls the repeated warm-launch scans in this fixture.

After promoting the default, both modes are rerun across 0/128/256/512 siblings
with a fresh build and matching per-size snapshot receipts. This covers both
sides of the previous enumeration pagination thresholds. The final sweeps are
`timestamps-default-density*.json` and `timestamps-zero-density*.json`.
`timestamps-store.json` and `timestamps-zero.json` retain the initial reversal.

All eight final runs completed with matching source/build/binaries and per-size
snapshots. At 512 siblings, the final zero → default reversal is 20.169 →
3.479 ms for scripts and 10.969 → 2.600 ms for binaries. Across all sizes,
zero-mode callback counts are 4/2, 4/2, 8/4 and 12/6 for script/binary launches;
the fixed mode records zero in every measured direct-launch sample, including
blocking-wait and explicit posix_spawn controls. The final pair of sweeps
contains 11,520 timed operations. The earlier directory sweep and initial
timestamp pair add 8,640 operations, all with completed cleanup gates.

## How the directory controls narrowed the problem

The initial `directory-controls*.json` sweep uses the old zero timestamps.
It adds two cases to the permanent launch suite:

- `native-listdir`: a timed directory listing, checked against an independent
  host name/count oracle after timing. Duplicate or missing names fail.
- `native-bundle-discovery`: public `CFBundleCreate`, info-dictionary retrieval
  and executable-URL discovery on the plain fixture directory. Owned objects
  are released, and incorrectly identifying an executable bundle fails.

At 512 siblings, native listing takes 2.467 ms versus fuser's 21.804 ms. Native
receives three enumeration callbacks and reads no repository directories;
fuser performs six repository-directory reads. Native is already faster at
this operation, so the warm launch result cannot be explained simply by slower
directory reads in native FSKit.

Repeated public bundle discovery takes 0.061 ms on native and causes no
enumeration callbacks. The Security API control still causes six callbacks and
takes 2.912 ms. Public bundle discovery in this warmed process therefore does
not reproduce the private discovery path exercised by Security.

Apple's published [Security implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/diskrep.cpp)
probes bundle membership, and its published
[CoreFoundation implementation](https://github.com/apple-oss-distributions/CF/blob/main/CFBundle.c)
uses executable lookup that ignores its ordinary cache. The earlier
[stack sample](../2026-09-16-native-fskit-enumeration/README.md)
confirms enumeration through Security on this Mac. These source observations
explain why the public API control is insufficient; they do not identify the
system service responsible for each process launch.

The direct Security API remains slower on fuser even after the native fix. It
must not substitute for actual process-launch measurements.

## Limits and next work

First execution after mounting remains expensive: the fixed sweep records
167–294 ms for scripts and 151–157 ms for binaries. These are individual
observations after correctness reads, not a controlled cold-cache distribution.
The fix addresses warm-launch reuse; it does not establish a first-launch gain.

Prefer a dedicated first-execution benchmark next, with fresh mounts and
separate repository-read, code-signing and process-start phases. Alternatively,
isolate mtime, ctime, birth and added time to narrow an upstream question about
zero-timestamp handling. No claim of a general FSKit defect or production
migration readiness follows from this one fixture.

## Permanent reproduction and validation

The listing and public bundle cases run in `native-fskit-launch` and its
density suite. `native-fskit-launch-zero-times` is registered in the manifest
and `benchmark all`. Both runners accept `--item-timestamps store|zero`.
All modes retain byte/listing, execution, xattr/mutation/sandbox denial,
publication, independent-mount and teardown gates.

From the configured Mac checkout with the native toolchain environment:

```sh
python3 -m benchmarks.suites.native_fskit_launch_density \
  --profile standard --repetitions 3 --output results/timestamps-default-density.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_launch_density import main
r = json.load(open('results/timestamps-default-density-0.json'))
raise SystemExit(main([
    '--profile', 'standard', '--repetitions', '3', '--item-timestamps', 'zero',
    '--bundle', r['bundle'], '--server-binary', r['server_binary'],
    '--output', 'results/timestamps-zero-density.json',
]))
PY
```

The initial listing/discovery sweep used the same density command before the
timestamp change. The initial reversal used the single launch runner at 512
siblings with `--item-timestamps store`, then `zero`, reusing its app/server.
Run `python3 benchmarks/reports/2026-09-18-native-fskit-directory-controls/summarize.py`
to verify retained receipts and reproduce timing/callback summaries.

The standalone [trace reproducer](../../tools/reproduce_bundle_scans.py)
uses only Python's standard library and an existing executable path. It repeats
the permanent Security case without mounting anything or making timing claims.
It can be used with filesystem counters or a rootless process stack sample.
The standalone script was also checked on the Mac with three successful
code-object creations for `/bin/echo`.

Local checks: 30 Python tests and three Rust library tests with the `repository`
feature pass. The Rust test checks the new default and zero-mode override;
mounted runs verify the actual timestamps. Formatting and whitespace checks
pass. Native release builds exercise the changed Objective-C timestamp setters.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
