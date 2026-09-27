# FSKit read callback phases

This experiment extends the permanent `native-fskit-workloads-read-trace`
benchmark, already included in `benchmark all`. It measures the real Rust
FSKit extension, production fuser/FUSE-T, and host execution on the same Mac.
There are three repetitions at 1, 17, 32, and 33 workers for both shared and
distinct executable paths. The ordinary permanent matrix also retains
1/8/15/16/17/31/32/33 workers.

## Results

All 72 trials and 2,988 workload executions passed, including exact final
callback coverage, output checks and cleanup. First-launch medians in ms:

| Paths / workers | Native | fuser | Host | Backend service | Copy | Reply |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Shared / 1 | 398.4 | 1,065.2 | 234.5 | 12.8 | 4.1 | 2.0 |
| Shared / 17 | 473.0 | 1,136.0 | 272.7 | 19.8 | 11.2 | 5.2 |
| Shared / 32 | 526.3 | 1,285.9 | 309.9 | 22.2 | 15.6 | 7.9 |
| Shared / 33 | 521.6 | 1,310.4 | 319.5 | 24.4 | 15.3 | 7.7 |
| Distinct / 1 | 410.5 | 1,061.4 | 239.1 | 14.7 | 4.4 | 2.1 |
| Distinct / 17 | 3,448.0 | 17,308.0 | 2,392.2 | 141.0 | 39.6 | 21.8 |
| Distinct / 32 | 6,301.1 | 31,589.5 | 4,389.1 | 266.2 | 72.5 | 40.8 |
| Distinct / 33 | 6,822.0 | 31,853.1 | 4,526.3 | 648.7 | 75.6 | 43.2 |

Copy and synchronous reply delivery are small relative to the remaining launch
delay. At 17 shared-path workers they sum to about 16 ms, with another 20 ms
in the backend, against a 473 ms batch. Directory reads also cost about 56 ms;
these measurements do not account for all metadata callbacks or framework work.
Do not interpret the difference as a measurement of executable validation.

The 32-to-33 distinct-path cliff remains: median opens rise from 32 to 85,
evictions from 0 to 53, and backend service from 266 to 649 ms. Copy/reply
durations grow gradually. Native is about 2.4x faster than fuser for shared
17-worker launches and 5.0x for distinct 17-worker launches in this run, but
still slower than host execution. This experiment adds instrumentation rather
than a launch optimization. Host load ranged from 1.84 to 7.60.

The retained [raw report](workloads-callbacks.json.gz) includes source and binary
hashes, environment and lifecycle receipts. [Summary](summary.txt) contains all
counts and medians. Local harness validation passed 23 tests; the real Mac run
compiled and exercised the extension and baseline. Formatting and whitespace
checks passed.

## Measurement

Each traced regular-file read callback records backend call time, buffer-copy
time (including disposal of the returned byte vector), and synchronous reply
call time. Diagnostic-file reads are excluded. Argument checks, backend-handle
acquisition, framework dispatch, and final metric accounting are outside these
timers. Backend time includes read-range instrumentation. Reply timing ends
when the reply block returns; it does not measure downstream kernel processing.
Concurrent service sums do not partition elapsed launch time.

The bounded five-counter aggregate has no per-callback allocation. It is
updated after reply completion, so live statistics may show fewer completed
callbacks than backend reads. Unmount requires exact coverage, no read errors,
zero resident readers, and repository protection release. Existing byte/range,
EOF, mmap, executable-output, mutation and lifecycle gates remain enabled.

Distinct paths cycle through three GNU tool binaries. They exercise file
identity pressure, not 33 unique content digests. Shared libraries and stdin
remain on the host. Fresh mounts and host file copies do not clear OS caches.
Execution order rotates across repetitions; this is a shared CI machine.

## Reproduce

On the existing Mac checkout, with the exact GNU tool fixture pinned:

```sh
source /Users/hetzner/casita-native-fskit.w9GGjK/environment.sh
cd /Users/hetzner/casita-native-repository.hqI8w3
export CASITA_NATIVE_TARGET_DIR=/Users/hetzner/casita-native-repository.hqI8w3/target
python3 results/pin-fixture-tools.py
python3 -m benchmarks.reports.2026-09-19-fskit-callbacks.measure
```

The pinning helper and exact tool receipts are retained in the preceding
[adaptive-cache report](../2026-09-19-adaptive-cache/README.md). For another
machine, set `CASITA_WORKLOAD_AWK`, `CASITA_WORKLOAD_SORT`, and
`CASITA_WORKLOAD_GZIP` to real executable binaries, then run:

```sh
python3 -m benchmarks.suites.native_fskit_workloads --profile standard \
  --repetitions 3 --metadata-files 0 --trace-read-ranges \
  --workload-workers 1 17 32 33 --output results/workloads-callbacks.json
```

Validate the retained report and regenerate the numerical summary from the
repository root:

```sh
python3 -m benchmarks.reports.2026-09-19-fskit-callbacks.summarize
```

## Next steps

Profile time outside the measured read callback phases, including other FSKit
operations, framework dispatch and executable validation, before attributing
the remaining host gap to any one component. Repository-scoped reader sharing
by `(content_key, size)` is another option for the 33-path cliff: the view already
exposes this key. Keep seek serialization, bounded residency and release gates;
add genuinely distinct-content executables alongside duplicate-content files
before evaluating that change.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
