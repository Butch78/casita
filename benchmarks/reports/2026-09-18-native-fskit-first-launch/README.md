# First execution and bounded reader reuse, 2026-09-18

## Change

The native repository prototype now retains up to 16 seekable immutable-file
readers. A first binary launch previously reopened its blob 103 times for 103
read callbacks. Reader reuse serves those callbacks with one blob open.

The cache uses least-recently-used eviction and a per-file mutex for seek/read
position. Different files do not hold the global cache lock during I/O. An
in-flight read retains its reader if its slot is evicted; the limit is 16
resident slots, not a strict bound on all in-flight handles or buffer bytes.
Read failures discard the stream. Flush clears cached holds before the
repository release barrier, with the runtime still alive.

The production fuser frontend remains unchanged. `--reader-cache disabled`
retains its old native per-read-open counterpart through a private repository
marker. The same option works in ordinary launch and density runners.

## Fresh-mount methodology

Earlier first-execution observations followed byte correctness checks. The new
permanent suite instead uses a fresh mount for each backend/target/preparation
trial. Host controls use fresh copied file identities. It does not purge shared
OS, code-signing or backing-store caches and is not a machine-cold benchmark.

Each three-repetition run contains 54 trials:

- Native, production fuser and host backends.
- Direct script and native executable targets, each on a distinct mount/path.
- No preparation, a full executable read, or `SecStaticCodeCreateWithPath`.
- First execution and an immediate second execution, each with output/exit gates.

Backend and case order rotate across repetitions. Setup, preparation, first
spawn, time until exit, and immediate second execution are recorded separately.
Counters surround preparation and each launch. Full listing and byte/range/mmap
validation runs afterward. The enclosing suite still requires ordinary mutation,
sandbox, publication, independent-volume and busy-unmount gates. Every fresh
mount must detach and pass its repository release barrier.

Setup is measured through the native mount command or fuser helper's ready
handshake; host setup includes copying the full fixture. It excludes build and
registration and does not measure cold extension-process startup. Do not compare
host copy time with mount-command time as equivalent work.

## Initial attribution

`first-launch-512.json` predates reader reuse. All 54 trials completed at 512
metadata siblings. Medians of three trials, in milliseconds:

| Unprepared first execution | Native | fuser | Fresh host file |
| --- | ---: | ---: | ---: |
| Script | 236.46 | 1142.09 | 154.91 |
| Binary | 761.04 | 1402.91 | 170.34 |

For native binaries, 103 blob opens consume hundreds of milliseconds. A full
read before launch costs 43.84 ms; the following launch falls to 194.63 ms. That
control demonstrates a filesystem-read cost without treating preparation as
free. Creating a Security code object reads only part of the binary and leaves
101 blob opens for launch, so it does not solve this cost.

Fuser's unprepared launch also makes 103 blob opens and performs 563 repository
directory reads. Its Security preparation moves much of the directory work
earlier, but preparation plus launch remains expensive. The native reader change
does not address fuser's separate directory-read behavior.

## Reader-reuse comparison

`first-launch-readers-enabled.json` and `first-launch-readers-disabled.json`
use matching source, build, native/fuser binary and snapshot receipts. Enabled
runs first, disabled second. Both retain the one-second immutable timestamps
from the preceding warm-launch fix. The host is a shared M1 Mac running macOS
26.6.2 under UID 501; no root or security-policy changes are used.

Both final runs completed all 54 trials, their enclosing mounted gates and all
three pressure cases. The reverse control restores the expensive binary launch:

| Unprepared native first execution | Reuse disabled | Reuse enabled | Blob opens, disabled → enabled |
| --- | ---: | ---: | ---: |
| Script | 227.899 ms | 220.117 ms | 1 → 1 |
| Binary | 778.339 ms | 248.977 ms | 103 → 1 |

That is an observed 68% reduction in first binary-launch latency. Time in native
blob opens falls from 420.043 ms to 4.341 ms. Directory and process-start costs
also vary between runs, so the open-time difference is not an exact additive
accounting of the whole latency change. Every final report matches the current
source receipts; every native trial releases all cached slots at teardown.

With reuse enabled, unprepared first execution medians are 220.117 ms for scripts
and 248.977 ms for binaries. Fuser measures 1185.288 ms and 1444.740 ms;
fresh host files measure 151.924 ms and 169.210 ms. Binary read callbacks remain
at 103, but blob opens fall to one. The script already needed only one blob
open, so no comparable script improvement is expected from this change.

Native mount readiness is about 82–83 ms in these unprepared trials, measured
separately. Immediate second executions are 12.049 ms for scripts and 7.062 ms
for binaries. These are not the fully warmed steady-state measurements from
the ordinary launch suite.

The enclosing steady-state controls still pass: enabled native warm script and
binary launch medians are 3.499 ms and 2.353 ms, with zero enumeration callbacks
in every measured sample. The preceding timestamp fix remains effective.

Reading the binary first takes 48.345 ms with reuse enabled, followed by a
180.579 ms launch; median preparation plus launch is 228.925 ms. This is a
diagnostic control, not a new eager-read policy. Public Security preparation
plus binary launch is 271.460 ms, so it provides no demonstrated total-time win.

Fresh host files still exhibit a substantial first-execution cost. The remaining
time cannot all be attributed to FSKit, and these measurements do not identify
the responsible macOS component. Precise speed ratios remain subject to
shared-host variation; the 103-to-1 blob-open reduction is direct evidence.

## Capacity boundary and correctness

Both cases include the permanent `reader-cache` helper workload: 20 cyclic
passes over 15, 16 or 17 nonempty boundary files/script. It bypasses the OS page
cache by reading through the actual Backend, checks every returned byte/range,
asserts expected open counts and verifies cached holds are released by flush.

| Active files | Disabled blob opens | Enabled blob opens | Disabled elapsed | Enabled elapsed |
| --- | ---: | ---: | ---: | ---: |
| 15 | 300 | 15 | 429.921 ms | 35.547 ms |
| 16 | 320 | 16 | 479.958 ms | 44.423 ms |
| 17 | 340 | 340 | 551.008 ms | 524.677 ms |

This deliberately covers the eviction cliff. A 17-file round-robin workload
thrashes a 16-slot cache. This is an explicit prototype limit, not a universal
cache-size recommendation. Same-file seeks are serialized; concurrent-read
throughput and larger working sets need separate evaluation before integration.

Rust tests cover interleaved concurrent seeks, EOF/range behavior, eviction and
reopening. Three repository-feature library tests and 32 targeted Python tests
pass. Native release compilation exercises the changed backend and helper.

## Permanent commands

`native-fskit-first-launch` and `native-fskit-first-launch-uncached` are registered
in `benchmarks/manifest.json` and included in `benchmark all`. Smoke uses three
cycles for the pressure cases; standard uses 20. Both preserve the capacity
boundary and the complete preparation/backend/target matrix.

On the configured Mac checkout with the documented toolchain environment:

```sh
python3 -m benchmarks.suites.native_fskit_first_launch --profile standard \
  --repetitions 3 --metadata-files 512 --output results/first-launch-readers-enabled.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_first_launch import main
r = json.load(open('results/first-launch-readers-enabled.json'))
raise SystemExit(main([
    '--profile', 'standard', '--repetitions', '3', '--metadata-files', '512',
    '--reader-cache', 'disabled', '--bundle', r['bundle'],
    '--server-binary', r['server_binary'],
    '--output', 'results/first-launch-readers-disabled.json',
]))
PY
```

Run `python3 benchmarks/reports/2026-09-18-native-fskit-first-launch/summarize.py`
to verify the retained receipts and reproduce all phase/counter tables.

Next, prefer representative application and concurrent working-set benchmarks
before production integration. Alternatively, investigate the remaining common
first-execution cost on ordinary host files, without treating it as FSKit-only
overhead or weakening macOS security.

## Compressed raw reports

Large JSON reports are stored as `.json.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.json.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
