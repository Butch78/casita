# Concurrent first-launch read ranges, 2026-09-18

## Question and method

The preceding real-tool benchmark found exactly 30 additional native read
callbacks for every added process launching the same GNU awk executable on a
fresh mount. This investigation records the requested ranges and separates
reader-lock waits, seeks and stream reads. It keeps the same shared/distinct
awk/sort/gzip workload, 1/8/15/16/17 workers, immediate repeats, fuser and host
controls, byte-output oracles, and mounted correctness/lifecycle gates.

Tracing is opt-in. The normal read path skips the diagnostic clocks and map.
The trace retains at most 8,192 inode/offset/request-size keys per mount, counting
calls, returned bytes, elapsed service nanoseconds and errors. A separate counter
records calls omitted at capacity. Traced trials must account for every callback,
with no omitted calls or errors, before they can pass. Phase times are aggregate
service times, not exclusive wall-time partitions across concurrent requests.
The cached-reader phases are timed only when reader reuse is enabled. Batch
wall time includes trace recording; backend service time excludes updating the
range histogram itself.

## Findings

The initial range run and all three phase-measurement repetitions reproduce all
30 additional ranges exactly at 8, 15, 16 and 17 workers. Each additional process requests:

- 17 ranges of 16,384 bytes;
- 12 ranges of 32,768 bytes;
- one range of 23,040 bytes;
- 694,784 bytes in total.

The last request extends 64 bytes past EOF, so the backend correctly returns
22,976 bytes there: 694,720 additional bytes returned per process overall.

The ranges span `__TEXT`, `__DATA_CONST`, `__DATA` and `__LINKEDIT`. The last
range includes the code signature, but these are not merely header or signature
rereads. `workloads-macho.json` retains the measured binaries' SHA-256 hashes and
file layout, parsed by `macho.py` using the Mach-O structures in the Mac SDK's
`mach-o/loader.h`. Zero-fill sections are marked as having no file backing.

One fresh worker makes 281 callbacks. Seventeen make 761. Immediate repeats make
zero callbacks in the initial range run. The instrumentation identifies the
ranges and service costs; it does not identify the macOS caller or distinguish
kernel page-in requests from other callers solely from their offsets.

Median first-batch measurements across three traced repetitions, milliseconds:

| Workers sharing awk | Callbacks | Batch wall | Backend service | Reader-lock wait | Seek | Stream read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 281 | 374.897 | 17.719 | 0.017 | 0.143 | 10.644 |
| 8 | 491 | 606.527 | 248.331 | 0.029 | 3.526 | 236.510 |
| 15 | 701 | 862.171 | 454.082 | 0.040 | 7.260 | 439.607 |
| 16 | 731 | 902.261 | 484.352 | 0.040 | 7.175 | 470.744 |
| 17 | 761 | 935.584 | 525.440 | 0.043 | 8.002 | 506.417 |

Reader-lock contention is negligible in this workload. The additional cost is
predominantly inside the stream reads following seeks. The seek call itself is
cheap; deferred fetching and decoding can happen during the subsequent read.
Stream-read timing includes allocating the output buffer and filling it.
Backend service also includes lookup, runtime entry, cache management and opening
readers. Columns are independent medians and need not add up.

The source explains a plausible expensive path: a nonsequential seek in
`src/blob/chunked_reader.rs` rebuilds the stream from the target chunk. Fetching
that chunk can perform decoding again, including through the packed reader in
`src/blob/pack/fetch.rs`. Range and phase counters alone do not prove how much of
the stream-read time is decompression, verification, fetching or scheduling.

## Artifacts and validation

The phase run and matching untraced control both completed all 90 trials and
2,052 checked child executions per run. Including the initial range run, this
investigation completed 210 trials and 4,788 checked child executions. All
correctness and teardown gates passed. The two final reports match on source,
build, filesystem binaries, server, snapshot and tool/input receipts, and match
the current instrumented sources.

Selected native first-batch medians, milliseconds:

| Pattern | Workers | Tracing disabled | Tracing enabled |
| --- | ---: | ---: | ---: |
| Shared | 1 | 360.151 | 374.897 |
| Shared | 8 | 594.546 | 606.527 |
| Shared | 17 | 903.279 | 935.584 |
| Distinct | 17 | 3659.047 | 3576.390 |

The shared-file callback counts are identical between controls. At 17 shared
workers, the traced median is 3.6% higher. The distinct-file traced median is
lower, illustrating shared-host and run-order variation. Tracing ran first;
these sequential comparisons do not isolate instrumentation overhead from drift.
`summary.txt` retains all process counts and phase/range summaries.

- `workloads-read-trace.json`: rejected initial diagnostic run. The old synthetic
  statistics file truncated JSON at 8 KiB. Cleanup passed; it is not a timing result.
- `workloads-read-ranges.json`: complete initial range run, 30 trials and 684
  checked child executions, with no dropped ranges or read failures.
- `workloads-read-phases.json`: repeated range and phase measurements.
- `workloads-read-control.json`: matching build with tracing disabled.
- `workloads-macho.json`: executable file-layout receipts.
- `summarize.py`: reconstructs callback deltas, timing phases and extra ranges.

Traced synthetic statistics files now have a 2 MiB capacity. Ordinary files keep
their existing 8 KiB capacity. Both reject overflow explicitly instead of silently
truncating JSON. Native filesystem data-serving behavior is unchanged.

All 36 targeted Python tests and all three native Rust library tests pass,
including range/EOF instrumentation and concurrent reader correctness. Runs use
UID 501 without root, and retain the native no-TCP-listener check. The shared
M1 Mac runs macOS 26.6.2; absolute timings remain subject to shared-host load.

## Permanent reproduction

`native-fskit-workloads-read-trace` is registered in `benchmarks/manifest.json`
and `benchmark all`. Smoke retains the complete matrix with one repetition;
standard uses three. `native-fskit-workloads` supplies the untraced control.
Select executable tool binaries as documented in the
[workload report](../2026-09-18-native-fskit-workloads/README.md), then run from the
configured Mac checkout:

```sh
python3 -m benchmarks.suites.native_fskit_workloads --profile standard \
  --repetitions 3 --metadata-files 0 --trace-read-ranges \
  --timeout-seconds 2400 --output results/workloads-read-phases.json
python3 - <<'PY'
import json, os
from benchmarks.suites.native_fskit_workloads import main
r = json.load(open('results/workloads-read-phases.json'))
os.environ.update({'CASITA_WORKLOAD_' + k.upper(): v['source']
                   for k, v in r['workload_fixture']['tools'].items()})
raise SystemExit(main([
    '--profile', 'standard', '--repetitions', '3', '--metadata-files', '0',
    '--timeout-seconds', '2400', '--bundle', r['bundle'],
    '--server-binary', r['server_binary'],
    '--output', 'results/workloads-read-control.json',
]))
PY
python3 benchmarks/reports/2026-09-18-native-fskit-read-ranges/summarize.py
```

Next, prefer a direct replay of these measured ranges with chunk-fetch/decode
counters, then test retaining decoded chunks across nearby seeks under a bounded
memory budget. An alternative is sampling the native extension during first
launch to separate fetching, decompression and verification costs before changing
storage behavior. Neither requires removing integrity checks or root access.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
