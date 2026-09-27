# Real command-line tools and concurrent launches, 2026-09-18

## Scope

This extends the small Rust launch fixture to GNU awk, GNU sort and GNU gzip.
The executable files reside on native FSKit, production fuser/FUSE-T or a host
directory. Each program processes the same 147,456-byte input through stdin.
Shared libraries remain on the host. This tests executable startup and useful
command-line work, not an entire mounted application dependency closure.

Two patterns run at 1, 8, 15, 16 and 17 concurrent processes:

- **Shared:** all workers execute the same awk file.
- **Distinct:** independent executable paths cycle through awk, sort and gzip.
  There are up to 17 file identities, but only three application binaries.

Each trial uses a fresh mount or fresh host file identities, runs a first batch,
then immediately repeats it. A thread barrier starts workers together; reported
batch wall time runs from barrier release through the last child completion.
Thread setup and output validation are excluded from that interval. Per-process
spawn and completion times are also retained. This does not purge shared OS,
code-signing or repository backing-store caches.

## Cached implementation results

Median batch wall time over three repetitions, seconds. These are total batch
times, not per-process latency:

| Pattern | Processes | Native first | fuser first | Host first | Native repeat | fuser repeat | Host repeat |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Shared | 1 | 0.384 | 1.050 | 0.231 | 0.024 | 0.028 | 0.022 |
| Shared | 8 | 0.594 | 1.103 | 0.256 | 0.053 | 0.043 | 0.042 |
| Shared | 15 | 0.831 | 1.145 | 0.278 | 0.080 | 0.083 | 0.078 |
| Shared | 16 | 0.899 | 1.144 | 0.272 | 0.087 | 0.082 | 0.084 |
| Shared | 17 | 0.920 | 1.166 | 0.264 | 0.091 | 0.091 | 0.091 |
| Distinct | 1 | 0.350 | 1.055 | 0.222 | 0.027 | 0.027 | 0.023 |
| Distinct | 8 | 1.677 | 8.395 | 1.166 | 0.070 | 0.045 | 0.070 |
| Distinct | 15 | 3.056 | 14.319 | 2.108 | 0.103 | 0.101 | 0.103 |
| Distinct | 16 | 3.173 | 16.007 | 2.248 | 0.107 | 0.090 | 0.100 |
| Distinct | 17 | 3.680 | 16.818 | 2.390 | 0.111 | 0.104 | 0.102 |

Native wins these first-batch comparisons against fuser. Immediate repeats are
much closer across implementations; native is not consistently faster than
fuser there. The shared-file first batch still has substantial overhead against
the host, especially at higher concurrency. The shared M1 Mac ran macOS 26.6.2
under UID 501. Treat absolute timings as shared-host observations.

## Cache pressure and the remaining shared-file cost

Both complete runs passed: 180 trials and 4,104 checked application executions,
with matching source/build/binary, snapshot, tool and input receipts. Median
native first-batch wall times in seconds:

| Pattern | Processes | Reader reuse disabled | Reader reuse enabled | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Shared | 1 | 1.935 | 0.384 | 5.0x |
| Shared | 8 | 3.177 | 0.594 | 5.3x |
| Shared | 15 | 4.457 | 0.831 | 5.4x |
| Shared | 16 | 4.689 | 0.899 | 5.2x |
| Shared | 17 | 4.831 | 0.920 | 5.3x |
| Distinct | 1 | 1.949 | 0.350 | 5.6x |
| Distinct | 8 | 13.080 | 1.677 | 7.8x |
| Distinct | 15 | 22.436 | 3.056 | 7.3x |
| Distinct | 16 | 24.220 | 3.173 | 7.6x |
| Distinct | 17 | 26.474 | 3.680 | 7.2x |

Reader reuse changes the practical comparison: without it, native first batches
are slower than fuser throughout this matrix. With it, native first batches are
faster throughout. At 17 distinct paths, reuse reduces median wall time by 86.1%
and blob opens from 3,922 per trial to 36–39. Immediate repeats remain broadly
similar between cache modes. The runs are sequential, with enabled first, so
these are measured workload comparisons rather than randomized causal estimates.

The fuser `reads` counter does not count its production view's read path. Its
`blob_opens` counter is useful here; zero reported `reads` does not mean zero IO.

Distinct-path native first batches open one reader per file through 16 files.
At 17 files, the three trials make 39, 36 and 39 blob opens, with 3,922–3,952 read
callbacks. Eviction increases work but does not reproduce the all-miss behavior
of the deliberately cyclic 17-file pressure microbenchmark. That microbenchmark
remains useful as a worst-case control. The 15/16/17 real-tool cases permanently
cover both sides of the cache capacity boundary.

Shared-path native first batches retain one blob reader at every concurrency.
Read callbacks nevertheless grow from 281 at one worker to 491/701/731/761 at
8/15/16/17 workers: exactly 30 extra callbacks per additional process in all
three repetitions. Immediate repeat batches make zero native read callbacks.
This is a remaining first-batch cost independent of reader eviction. We have
not yet attributed the repeated ranges or their macOS caller. These counts do
not justify attributing the entire latency difference to the callback itself.

## Fixture selection and correctness

The first attempt copied Apple's `/usr/bin/awk`, `/usr/bin/sort` and
`/usr/bin/gzip`. The copied awk process exited with SIGKILL during the host
preflight, before repository import or mounting. `workloads-apple-rejected.json`
retains the failed gate; it is not a performance result. The cause of that kill
was not diagnosed, and no system security policy or code signature was changed.

The measured fixtures are ordinary arm64 development binaries from the Mac's
existing Nix store. The selected sort command is provided by a coreutils
multicall executable, so the harness sets argv[0] to `sort` while using the
mounted executable path. The gzip package's wrapper would redirect execution
to a host binary; the benchmark selects its underlying executable instead and
rejects shebang wrappers. Tool sources, byte sizes and SHA-256 digests, plus the
input hash, are retained in `workload_fixture` in each report.

Every child must exit successfully and pass its output oracle: exact sorted
bytes, exact numbered lines, or a gzip decompression round trip. The copied
programs pass these gates on the host before import. Complete mounted listing,
byte/range and mmap checks run after each measured batch pair. Ordinary sandbox,
mutation, publication, independent-volume and unmount gates also remain required.
Every native trial releases all cached readers before its repository barrier.

`workloads-enabled.json` and `workloads-disabled.json` compare the reader-reuse
control with matching source/build/binary, snapshot, tool and input receipts.
The enabled case runs first; backend and case order rotate across repetitions.
Each complete run contains 90 fresh-path trials and 2,052 checked application
executions. The production fuser frontend is unchanged by the reader control.

## Permanent reproduction

`native-fskit-workloads` and `native-fskit-workloads-uncached` are registered in
`benchmarks/manifest.json` and `benchmark all`. Smoke keeps the full process-count
and pattern matrix with one repetition. Standard measurements below use three.
The new harness has tests for all three output oracles, simultaneous workers,
distinct/shared paths, wrapper rejection and missing/incomplete trials. All 36
targeted Python tests pass. Native release builds and mounted gates validate the
integration; the Rust filesystem implementation is unchanged in this experiment.

From the configured Mac checkout after sourcing its toolchain environment:

```sh
export CASITA_WORKLOAD_AWK=/nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk
export CASITA_WORKLOAD_SORT=/nix/store/26h13pzgcz97dc7x11nbvd2h6g0j6dk5-coreutils-full-9.11/bin/sort
export CASITA_WORKLOAD_GZIP=/nix/store/3fdibjln064bpvcgnn4lb5fla4rfr6ip-gzip-1.14/bin/.gzip-wrapped
python3 -m benchmarks.suites.native_fskit_workloads --profile standard \
  --repetitions 3 --metadata-files 0 --timeout-seconds 2400 \
  --output results/workloads-enabled.json
python3 - <<'PY'
import json, os
from benchmarks.suites.native_fskit_workloads import main
r = json.load(open('results/workloads-enabled.json'))
os.environ.update({'CASITA_WORKLOAD_' + k.upper(): v['source']
                   for k, v in r['workload_fixture']['tools'].items()})
raise SystemExit(main([
    '--profile', 'standard', '--repetitions', '3', '--metadata-files', '0',
    '--reader-cache', 'disabled', '--timeout-seconds', '2400',
    '--bundle', r['bundle'], '--server-binary', r['server_binary'],
    '--output', 'results/workloads-disabled.json',
]))
PY
```

Other installations can supply their own executable paths with those environment
variables; hashes make any changed tool inputs visible. Set `CASITA_NATIVE_TARGET_DIR`
as described in the native evaluation setup when reusing the build directory.
Run `python3 benchmarks/reports/2026-09-18-native-fskit-workloads/summarize.py`
to verify retained receipts and regenerate the timing/counter summaries.

Next, prefer attributing the 30 extra reads per concurrent shared-file launch.
An alternative is testing a complete mounted toolchain dependency closure and
real build commands before making a production migration decision.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
