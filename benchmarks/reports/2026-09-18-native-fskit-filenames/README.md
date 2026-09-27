# Direct-byte filename construction, 2026-09-18

## Decision

Retain direct-byte construction as an opt-in experiment. It passed correctness,
but the uninstrumented sweep does not establish a consistent launch improvement.
The default remains the existing `NSData` path. No cache or production frontend
change was added.

Median of three round p50s, in milliseconds:

| Metadata siblings | Native script, data → bytes | Native binary, data → bytes | fuser script / binary, bytes run |
| --- | ---: | ---: | ---: |
| 0 | 9.861 → 9.112 | 5.936 → 5.152 | 4.783 / 6.546 |
| 128 | 12.368 → 12.033 | 7.653 → 7.200 | 5.454 / 6.968 |
| 256 | 16.605 → 17.675 | 10.189 → 10.832 | 5.167 / 7.407 |
| 512 | 32.601 → 20.790 | 15.887 → 13.389 | 5.395 / 7.289 |

The 512-sibling result is encouraging, but the 256-sibling result regressed.
Data mode ran first under load averages around 4.6–6.0, followed by bytes mode
around 3.2–3.4. This is a shared Mac; the observed differences cannot all be
attributed to the constructor. Host and fuser controls are retained per round.

Enumeration counts are unchanged: script/binary launches cause 4/2, 4/2, 8/4 and
12/6 callbacks across the four sizes. This covers both sides of the known
pagination steps. The constructor does not eliminate bundle-discovery scans.

## Implementation and gates

The opt-in path calls `FSFileName::initWithBytes_length` directly with the
borrowed Rust byte slice. FSKit copies the bytes. This avoids our temporary
`NSData` and the `nameWithData:` factory, but still creates an FSFileName per
packing attempt. Other filename uses retain their existing implementation.

Before exposing each native volume, both constructors must preserve empty,
ASCII, dot-underscore, invalid UTF-8, Unicode and 255-byte names after the source
buffer is overwritten and freed. These are constructor-level checks, not a
claim that the portable mounted fixture covers every raw filename. The existing
mounted listing, bytes, mmap, execution, mutation/sandbox denial, publication,
independent-mount and teardown gates remain required.

Both aggregate sweeps and all eight children completed. Each child contains
14 cases × three backends × three rounds × ten samples, for 10,080 timed
operations total. All use identical source/build/native/fuser binary identities;
each paired directory size uses the same snapshot. Per-entry phase timers are
disabled in these sweeps. The Mac release build and constructor gates passed;
41 Python and three Rust library tests pass, as do formatting and whitespace
checks. No root or system security-policy change was needed.

## Permanent reproduction

### Detailed reverse-order check

Two additional 512-sibling runs used the same build and snapshot with detailed
phase timers: bytes first, data second. Both passed all gates (2,520 additional
timed operations). Load averages were about 3.5–3.9 in these runs.

| Native operation | Filename construction, data → bytes | Packing, data → bytes | Total operation, data → bytes |
| --- | ---: | ---: | ---: |
| Script launch | 1.872 → 1.046 ms | 4.962 → 6.119 ms | 21.954 → 25.598 ms |
| Binary launch | 1.321 → 0.624 ms | 3.462 → 3.619 ms | 13.203 → 13.374 ms |
| Code-object creation | 0.368 → 0.135 ms | 1.344 → 1.345 ms | 3.025 → 2.840 ms |

Direct construction reduces the measured constructor cost. Packing remains the
larger cost, and total launch latency does not improve consistently. Timers and
shared-host variation limit precise attribution; constructor savings must not
be presented as an equivalent end-to-end launch saving. The default stays data.

### Commands

`native-fskit-launch-density-filename-bytes` is registered in the manifest and
`benchmark all`. Normal density mode retains `--filename-construction data`;
the new case selects `bytes`. Both modes are supported by the same compiled
backend. A private repository marker selects bytes mode on mount.

From the repository root on the Mac with the documented toolchain:

```sh
python3 -m benchmarks.cli run native-fskit-launch-density --profile standard \
  --repetitions 3 --filename-construction data --output results/filename-data.json
python3 - <<'PY'
import json
from benchmarks.suites.native_fskit_launch_density import main
r = json.load(open("results/filename-data-0.json"))
raise SystemExit(main([
    "--profile", "standard", "--repetitions", "3",
    "--filename-construction", "bytes",
    "--bundle", r["bundle"], "--server-binary", r["server_binary"],
    "--output", "results/filename-bytes.json",
]))
PY
```

Run `python3 benchmarks/reports/2026-09-18-native-fskit-filenames/summarize.py`
to validate matching identities, gates and callback counts and reproduce the
latency summary.

The detailed follow-up uses the same permanent launch runner:

```python
import json
from benchmarks.suites.native_fskit_launch import main
r = json.load(open("results/filename-data-0.json"))
for mode in ("bytes", "data"):
    assert main([
        "--profile", "standard", "--repetitions", "3", "--metadata-files", "512",
        "--enumeration-timing", "detailed", "--filename-construction", mode,
        "--bundle", r["bundle"], "--server-binary", r["server_binary"],
        "--output", f"results/filename-phases-{mode}.json",
    ]) == 0
```

## Next

Prefer addressing repeated bundle-discovery enumeration at the FSKit/framework
boundary, using the retained minimal workloads and stack traces as evidence.
Constructor changes and another filename cache can only reduce a fraction of
the measured callback work. An alternative is a quieter-host randomized repeat
of these constructor modes before deciding whether the smaller local savings
justify changing the default. Keep fuser in production in either case.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
