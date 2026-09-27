# Native enumeration phase attribution, 2026-09-18

## Finding

FSKit's directory-entry packer dominates the measured enumeration work. Creating
and releasing filename objects is meaningful but smaller. Reusing entry vectors
has already made setup cheap. A filename cache alone cannot remove most of the
remaining callback time or the repeated bundle-discovery callbacks.

Detailed native measurements, median per-operation phase times in milliseconds:

| Metadata siblings | Operation | Setup | Filename construction | Packing | Filename release | Whole callback |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 0 | Script launch | 0.010 | 0.109 | 0.288 | 0.019 | 0.449 |
| 128 | Script launch | 0.012 | 0.711 | 1.950 | 0.131 | 2.955 |
| 256 | Script launch | 0.022 | 1.388 | 3.739 | 0.248 | 5.677 |
| 512 | Script launch | 0.044 | 2.506 | 6.630 | 0.447 | 10.162 |
| 512 | Binary launch | 0.033 | 1.725 | 4.644 | 0.310 | 7.087 |

Each phase median is calculated separately, so columns need not add to the
median callback total. Loop bookkeeping, item construction if requested and
timer overhead are outside the named phases but inside the callback total.
Timers include work on the rejected entry when a packer fills; attempts and
accepted-entry counts are both retained. Filenames are released outside the
packing timer to distinguish object lifetime cost from packing.

At 512 siblings, even eliminating all filename construction and release would
only remove about 3 ms of measured work per script launch in this instrumented
run. A real cache has lookup, retention and memory costs, so this is a rough
ceiling from attribution, not a promised speedup. No filename cache was added.

## Permanent benchmark and correctness

Both detailed and basic sweeps completed: eight child runs, each with 14 cases,
three backends, three rounds and ten samples per case/backend/round. All 10,080
timed operations passed their result gates, and all runs passed teardown. Source,
build and binary identities match across runs; snapshots match at each size.
Source receipts also match the local implementation. Three Rust library tests,
41 Python tests, formatting and whitespace checks pass.

Basic-mode launch medians, with per-entry timers disabled:

| Metadata siblings | Native script | fuser script | Native binary | fuser binary |
| --- | ---: | ---: | ---: | ---: |
| 0 | 11.325 ms | 4.846 ms | 6.709 ms | 6.734 ms |
| 128 | 16.046 ms | 5.291 ms | 9.066 ms | 7.441 ms |
| 256 | 19.717 ms | 4.871 ms | 9.962 ms | 6.783 ms |
| 512 | 23.572 ms | 5.360 ms | 14.232 ms | 10.032 ms |

Detailed timing ran first, then basic timing, with rotating backends/cases inside
each child. These are shared-host observations on the same Apple M1 Mac. Load
was roughly 4.2–5.4 during detailed timing and rose to about 10.6 during the last
basic-mode child. At 512 siblings the detailed callback median was 10.162 ms per
script versus 6.292 ms in basic mode; for binaries it was 7.087 versus 3.138 ms.
This combination of instrumentation and host variation prevents a precise
measurement of clock overhead. Do not subtract the phase columns from basic
launch times to predict an optimization's effect. This turn changes diagnostic
instrumentation, not the underlying filename or packing implementation.

`native-fskit-launch-density-phases` is registered in `benchmarks/manifest.json`
and `benchmark all`. It enables `--enumeration-timing detailed` while the default
is `basic`, avoiding per-entry clock reads. Both exercise 0, 128, 256 and 512
metadata siblings, covering both sides of the known pagination steps. All
14 launch, path, xattr and code-object cases run against native, fuser and host.

The counters are grouped by directory, attributes requested and initial versus
continuation cookie. `native_enumeration_phases` contains setup ns, filename ns,
packing ns, filename-drop ns and pack attempts. Its bounded map follows the
existing enumeration instrumentation. Default-mode counters remain empty.

The report validator checks successful lifecycle/cleanup, nonnegative deltas,
phase totals bounded by callback time, and attempts bounded between accepted
entries and accepted entries plus callback count. Existing exact listing,
bytes, mmap, execution, mutation/sandbox denial, publication and independent
mount gates remain required. No root or security-policy changes are needed.

## Reproduce

Use the same Mac toolchain as the earlier reports, from the repository root:

```sh
python3 -m benchmarks.cli run native-fskit-launch-density-phases \
  --profile standard --repetitions 3 --output results/enumeration-phases.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_launch_density import main
r = json.load(open("results/enumeration-phases-0.json"))
raise SystemExit(main([
    "--profile", "standard", "--repetitions", "3",
    "--enumeration-timing", "basic",
    "--bundle", r["bundle"], "--server-binary", r["server_binary"],
    "--output", "results/enumeration-basic.json",
]))
PY
```

Run `python3 benchmarks/reports/2026-09-18-native-fskit-phases/summarize.py`
to verify and summarize the retained paired reports.

## Next

Prefer investigating why packing costs so much on this FSKit path, with a
rootless sample of our extension and a direct enumeration control. A filename
cache remains an alternative if the modest potential saving justifies retained
Objective-C objects and their lifecycle complexity. Keep fuser in production;
this investigation adds attribution, not a launch-speed fix.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
