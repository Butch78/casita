# Remaining native FSKit launch overhead — 2026-09-16

## Validated findings

**Launches repeatedly enumerate the executable's parent directory.** Native
script launch produces eight enumeration callbacks; native binary launch produces
four. These request no attributes and do not fetch repository data after the
previous directory-cache fix. Instrumentation now records callback count,
packed-entry count, elapsed time and item allocations, grouped by directory,
whether attributes were requested, and initial/continuation cookie.

The parent is inode 4, the fixture directory. These are plain enumeration
callbacks, not repeated missing-name lookups. This resolves the outstanding
callback-level attribution in the [previous investigation](../2026-09-15-native-fskit-launch/README.md).
It does not identify the macOS caller that initiates each enumeration.

The callback previously allocated an FSKit item and its full attribute object
for every packed entry even when the caller requested no attributes. It now
allocates them only when requested. A runtime control retains the old eager
allocation behavior for permanent regression comparisons.

Two correctness fixes accompany this work: plain enumeration includes `.` and
`..` (attribute enumeration excludes them), and the root's parent ID uses FSKit's
parent-of-root value 1. Both fixes are present on **both** sides of the allocation
comparison. The listing oracle now checks counts as well as the name set, so
duplicates cannot silently pass. See Apple's
[enumeration contract](https://developer.apple.com/documentation/fskit/fsvolume/operations/enumeratedirectory(_:startingat:verifier:attributes:packer:replyhandler:)).

## Same-binary allocation comparison

[Eager allocation](enumeration-eager.json.gz) and
[requested attributes only](enumeration-requested.json.gz) both completed
correctness, publication, isolation and teardown gates. Source hashes, binary
hashes, build identity and snapshot digest match. Three rounds, 10 operations per
case/backend/round, eight cases, native/fuser/host controls: 720 timed operations
per run, 24 paired comparisons. Six cases launch processes and two exercise path
operations directly. Times are medians of the three round p50s.

| Case | Native eager | Native requested only | fuser (requested-only run) |
| --- | ---: | ---: | ---: |
| Direct script, timed wait | 32.152 ms | 28.763 ms | 5.930 ms |
| Native executable, timed wait | 15.707 ms | 13.188 ms | 9.013 ms |
| Open + F_GETPATH + close | 0.041 ms | 0.045 ms | 0.872 ms |
| Open + close | 0.035 ms | 0.036 ms | 0.769 ms |

The change removes **1,332 item allocations per script launch and 666 per binary
launch** in this fixture. Median time inside our enumeration callbacks falls
from 10.05 to 7.59 ms for scripts, and 6.23 to 4.24 ms for binaries. Callback
counts stay at eight/four. This is a useful, modest improvement; it does not
explain or remove most of the remaining launch penalty.

The `F_GETPATH` syscall itself takes a median **3.4 µs** on native, causes no
enumeration callbacks, and is close to the host's 3.0 µs. Its whole operation
above includes open/close. Simple warm descriptor-to-path reconstruction is
therefore not the observed launch bottleneck. This control does not rule out
other path-resolution operations elsewhere in macOS.

Host: Apple M1, macOS 26.6.2, UID 501, same rootless setup. Load averages were
around 2.2–2.6 during these runs, lower than the earlier investigation. Backend
and case order rotate within each run; eager ran before requested-only. Treat
absolute timings as shared-host observations, not performance guarantees.

`before.json` is the first callback-instrumented run, before the dot-entry/root
parent corrections and path controls. It is retained for trace evidence, not
used as the same-binary allocation control.

## Extended-attribute experiment: no launch improvement

Mac access returned and both modes passed native release compilation, all
correctness/lifecycle gates and cleanup. [Emulated](xattrs-emulated.json.gz) and
[explicit](xattrs-explicit.json.gz) use identical source/build/binary identities and
snapshot digests. Each run has three rounds of ten samples per case/backend,
ten cases and native/fuser/host controls: 900 timed operations per run.

| Native operation | Emulated | Explicit |
| --- | ---: | ---: |
| Script launch | 22.911 ms | 24.497 ms |
| Binary launch | 11.131 ms | 12.330 ms |
| Get missing code-directory xattr | 0.017 ms | 0.187 ms |
| List empty xattrs | 0.008 ms | 0.196 ms |

**Explicit xattr handling did not eliminate enumeration:** scripts still cause
eight callbacks and binaries four at 256 metadata siblings. Direct xattr queries
cause no enumeration in either mode. Explicit native callbacks were exercised,
including code-signing and quarantine attribute reads; their counters are in the
reports. This rules out the proposed explicit-xattr change as a launch fix in
this configuration. Shared-host variation prevents attributing the small launch
slowdown precisely, but the direct query overhead is clear.

The emulated run's invocation controls further narrow the trigger. Direct script
execution with a blocking wait takes 19.037 ms and still causes eight callbacks;
`/bin/sh mounted-script` takes 3.909 ms and causes none. Native binary blocking
wait and explicit `posix_spawn` take 8.087 and 8.365 ms, both with four callbacks.
Changing Python's wait/spawn strategy does not remove the scans. This points to
the mounted-file execution path, rather than ordinary file reads or Python's
timeout handling; it does not yet identify the responsible macOS component.
Explicit interpretation is a diagnostic control, not a transparent replacement
for normal executable semantics.

Emulation remains the default. The explicit implementation stays opt-in as a
permanent diagnostic control: Casita snapshots contain no xattrs, so it returns
ENOATTR for reads, an empty list, and EROFS for mutation. Both modes passed the
new missing-attribute, empty-list and mutation-denial gates. The
[`xattrOperationsInhibited`](https://developer.apple.com/documentation/fskit/fsvolume/xattroperations)
control permits the same-binary comparison. The historical eight-case allocation
reports above predate these callbacks and controls; these new reports validate them.

## Density sweep and reproducibility

All diagnostic cases are permanent in the benchmark manifest and `benchmark all`.
The density suite measures 0, 128, 256 and 512 metadata siblings, using the same
compiled bundle and helper across sizes. Each directory also contains the
boundary files, script, executable, namespace marker, symlink and nested tree.
The emulated sweep completed all four sizes with identical binaries and full
gates. Directory size increases callback count in steps:

| Metadata siblings | Native script | Native binary | Script/binary enum callbacks |
| --- | ---: | ---: | ---: |
| 0 | 12.260 ms | 6.871 ms | 4 / 2 |
| 128 | 14.749 ms | 9.141 ms | 4 / 2 |
| 256 | 18.854 ms | 10.153 ms | 8 / 4 |
| 512 | 23.009 ms | 14.676 ms | 12 / 6 |

The corpus covers both sides of the observed pagination steps; the exact
transition counts are not yet isolated. These are callback counts, not duplicate
names returned to applications (the listing correctness gate passed).
The shared host varied substantially: host script medians ranged from 3.7 to
7.4 ms and fuser script medians from 4.8 to 12.4 ms. Therefore callback growth is
stronger evidence than a precise causal latency slope. Enumeration duration
itself grew from 0.52 to 9.58 ms per script launch between 0 and 512 siblings.

The earlier failed density attempt exposed omitted binary hashes in reused-bundle
reports. That reporting bug is fixed, covered by a unit test, and the complete
`density-emulated*.json` reports supersede that partial attempt.

The explicit-mode density sweep also completed and was retrieved after SSH
access returned. All four sizes passed all gates, with the same source, build,
binary and per-size snapshot identities as the emulated sweep. Callback counts
are identical in both modes: 4/2, 4/2, 8/4 and 12/6 for script/binary launches.
See `density-explicit*.json`; the earlier access interruption did not invalidate
the completed run.

From the repository root with the documented toolchain:

```sh
python3 -m benchmarks.cli run native-fskit-launch-eager --profile standard \
  --repetitions 3 --output /tmp/launch-eager.json
python3 -m benchmarks.cli run native-fskit-launch --profile standard \
  --repetitions 3 --output /tmp/launch-requested.json
python3 -m benchmarks.cli run native-fskit-launch-explicit-xattrs --profile standard \
  --repetitions 3 --output /tmp/launch-explicit.json
python3 -m benchmarks.cli run native-fskit-launch-density --profile standard \
  --repetitions 3 --output /tmp/density-emulated.json
python3 -m benchmarks.cli run native-fskit-launch-density-explicit-xattrs --profile standard \
  --repetitions 3 --output /tmp/density-explicit.json
```

Keep full correctness/cleanup gates. Compare the same snapshot and binary
identities between attribute modes, and retain both sides of any discovered
pagination or latency threshold in the density corpus. No root, security-policy
changes or altered application invocation are needed.

Local checks: 41 Python tests and three Rust library tests pass; formatting and
diff whitespace checks pass. Native release builds, mounted allocation comparisons and both xattr modes also
passed. Run `python3 benchmarks/reports/2026-09-16-native-fskit-enumeration/summarize.py`
to reproduce the retained timing/callback summaries and verify matching build identities.

## Path controls and bundle-discovery attribution

[Path controls](path-controls.json.gz) passed all gates with 13 cases, three rounds,
10 samples per backend. Native libc `realpath` took 0.213 ms, `getattrlist` name
0.0043 ms, and full-path 0.0047 ms. All caused **zero** enumeration callbacks.
Returned paths/names are checked against the fixture. These calls do not explain
the launch scans.

[Security controls](security-controls.json.gz) add `SecStaticCodeCreateWithPath`,
bringing the permanent launch suite to 14 cases. It must return success and a
non-null code object, with CF objects released after measurement. All three
backends passed; the complete run has 1,260 timed operations and full lifecycle
gates. On native it reproduces **four enumeration callbacks**, matching a binary
launch at the same directory size. Median code-object creation was 1.921 ms on
native, 27.072 ms on fuser and 0.331 ms on the host. This direct API call has a
different context/cache behavior from system launch handling: its latency is
not an additive estimate of launch overhead, and it does not establish that
native launches are faster than fuser.

A separate rootless [stack sample](security-profile-stacks.txt.gz) of our benchmark
process confirms the installed framework's path:

```text
SecStaticCodeCreateWithPath
  _CFBundleCreate
    _CFBundleGetBundleVersionForURL
      _CFIterateDirectory -> readdir -> __getdirentries64
    CFBundleGetInfoDictionary
      _CFBundleCopyInfoDictionaryInDirectoryWithVersion
        _CFIterateDirectory -> readdir -> __getdirentries64
```

The sampled benchmark contains both adapters and host controls; do not interpret
its stack percentages as native-only time. The [profiled run](security-profile.json.gz)
completed all 10 rounds and cleanup using the same build/snapshot as the security
control, but profiling perturbs timings, so it is retained for attribution only.

Apple's published [Security implementation](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/SecStaticCode.cpp)
constructs the code object through
[`DiskRep::bestGuess`](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/diskrep.cpp),
which probes whether the executable belongs to a bundle.
[CoreFoundation's bundle-layout detection](https://github.com/apple-oss-distributions/CF/blob/main/CFBundle_Resources.c)
iterates the directory looking for bundle layout names. The stack sample confirms
that this broad path is still present on the tested macOS release.

**Conclusion:** bundle discovery is a concrete, rootlessly reproduced source of
these parent-directory scans. Matching launch callback counts strongly implicate
it in the launch overhead. We have not sampled the actual system launch caller,
so identifying a specific daemon, or claiming this explains every millisecond,
would exceed the evidence. No signing, quarantine or system security policy was
changed.

The four new controls are included in the existing manifest-registered launch
and density runners and therefore in `benchmark all`. The older density reports
precede these additional controls; their source receipts intentionally differ.

Reproduce the current unprofiled controls:

```sh
python3 -m benchmarks.cli run native-fskit-launch --profile standard \
  --repetitions 3 --output results/security-controls.json
```

To reproduce the stack sample from the repository root, after that run:

```python
import json, subprocess, time
r = json.load(open("results/security-controls.json"))
with open("results/security-profile.log", "w") as log:
    process = subprocess.Popen([
        "python3", "-m", "benchmarks.cli", "run", "native-fskit-launch",
        "--profile", "standard", "--repetitions", "10",
        "--bundle", r["bundle"], "--server-binary", r["server_binary"],
        "--output", "results/security-profile.json",
    ], stdout=log, stderr=subprocess.STDOUT)
    time.sleep(20)
    subprocess.run(["sample", str(process.pid), "20", "1", "-file",
                    "results/security-profile-stacks.txt"], timeout=40, check=True)
    assert process.wait() == 0
```

Sampling uses only our own ordinary-user process. The benchmark retains all
correctness gates and its existing deadline and mount cleanup.

## Next

Prefer optimizing immutable-directory enumeration now that its repeated use is
reproduced: cached FSFileName objects or less metadata cloning are candidates,
with the existing eager/cache/density controls guarding semantics and regressions.
An alternative is deeper system-caller attribution to determine whether macOS
can avoid repeated bundle discovery for a correctly declared volume. Do not
advertise unsupported capabilities or change security policy to make launches
faster. Keep fuser in production until the complete native path meets the gates
and performance requirements.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
