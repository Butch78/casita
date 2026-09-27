# Volume capability control, 2026-09-18

## Result

Explicit volume capabilities did not eliminate repeated directory enumeration.
Keep the minimal default; the additional declarations remain an opt-in control.
Both runs used the same source, build, native and fuser binaries, and snapshot
on the shared M1 Mac running macOS 26.6.2, without root.

Median of three round p50s, milliseconds, with 512 metadata siblings:

| Operation | Native minimal | Native explicit | fuser minimal run | fuser explicit run | Native callbacks, both |
| --- | ---: | ---: | ---: | ---: | ---: |
| Direct script launch | 22.256 | 18.427 | 6.205 | 5.285 | 12 |
| Native binary launch | 12.002 | 9.729 | 8.111 | 6.650 | 6 |
| Static code-object creation | 3.009 | 2.888 | 46.221 | 45.083 | 6 |

Minimal ran first, explicit second. Both native and fuser launch times improved
by similar proportions, so these timings do not establish a causal speedup.
Callback counts are unchanged. This rules out these declarations as a way to
remove scans in this fixture, not every possible macOS capability interaction.
Host timings and all samples are retained in the JSON reports. The direct
code-object API has different performance from process launch on fuser and
must not be used as a substitute for the launch measurement.

## What macOS saw

The control declares case-sensitive names, 64-bit object IDs, fast statfs and
unavailable root timestamps through FSKit's supported volume capabilities.
An untimed `getattrlist(ATTR_VOL_CAPABILITIES)` gate verifies valid bits at the
VFS interface before timing. Format capability masks were:

| Backend | Minimal run | Explicit run |
| --- | ---: | ---: |
| Native | `0x302` | `0x20722` |
| fuser | `0x603` | `0x603` |
| Host APFS | `0x19b6edf` | `0x19b6edf` |

Native valid mask is `0xffffff`. Case sensitivity/preservation was already
present; the added bits are `0x20420`. Native persistent IDs remain unset:
the prototype allocates mount-local IDs. Apple's
[persistent-ID documentation](https://developer.apple.com/documentation/fskit/fsvolume/supportedcapabilities/supportspersistentobjectids)
does not justify declaring these persistent. The available
[capability API](https://developer.apple.com/documentation/fskit/fsvolume/supportedcapabilities)
provides no documented switch to skip bundle discovery.

Apple's published
[CFBundle resource code](https://github.com/apple-oss-distributions/CF/blob/main/CFBundle_Resources.c)
iterates directories while detecting bundle layout. Together with the earlier
[stack profile](../2026-09-16-native-fskit-enumeration/README.md), this motivates
the control. That published implementation is not proof of every detail of
the current OS, and we have not identified the system process initiating each
real launch scan.

## Gates and reproduction

Both runs completed all mounted correctness, lifecycle and cleanup gates.
Each contains 14 cases, three backends, three rounds and ten samples per case:
2,520 timed operations across the pair. Per-entry timers and sampling were off.
The explicit run reused the baseline app and server with source/build receipt
checks. The new capability parser has failure and malformed-reply tests;
42 Python tests and three Rust library tests passed.

`native-fskit-launch-capabilities` is registered in `benchmarks/manifest.json`
and `benchmark all`. The density runner also forwards the control, retaining
the existing 0/128/256/512 cases across the observed pagination thresholds.

On the configured Mac, from the checkout with the native build environment:

```sh
python3 -m benchmarks.cli run native-fskit-launch --profile standard \
  --repetitions 3 --metadata-files 512 --output results/capabilities-minimal.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_launch import main
r = json.load(open('results/capabilities-minimal.json'))
raise SystemExit(main([
    '--profile', 'standard', '--repetitions', '3', '--metadata-files', '512',
    '--volume-capabilities', 'explicit', '--bundle', r['bundle'],
    '--server-binary', r['server_binary'],
    '--output', 'results/capabilities-explicit.json',
]))
PY
python3 benchmarks/reports/2026-09-18-native-fskit-capabilities/summarize.py
```

The summarizer reads the retained JSONs beside it and verifies matching receipts.
For a fresh standalone explicit run, use `benchmark run
native-fskit-launch-capabilities`. Next, prefer a compact upstream reproducer
of repeated bundle scans. A quieter randomized comparison is an alternative
if a precise estimate of the declarations' latency effect is needed.

## Compressed raw reports

Large JSON reports are stored as `.json.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.json.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
