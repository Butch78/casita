# Decoded-chunk reuse for seek-heavy reads, 2026-09-18

## Change

`ChunkedReader` retains verified decoded chunks after its second nonsequential
seek. The cache is private to the reader and uses least-recently-used eviction,
with limits of 2 MiB and 64 entries. A chunk larger than the byte limit bypasses
the cache. Sequential reads and a single initial range seek do not populate it.
The cache key includes both chunk digest and manifest size.

Entries copy the verified bytes rather than retaining the source's `Bytes`
owner. That owner can carry a decoding permit and physical read plan. Keeping it
in a cache could block subsequent decodes or prolong storage pins. Eviction
precedes copying, and releasing the reader releases its cache. In-flight stream
buffers remain separate from the 2 MiB resident cache limit. This is a per-reader
limit, not a repository-wide memory budget.

Digest checks remain in the source; failed decodes never populate the cache.
Straight sequential reads retain their whole-blob EOF verification. As before,
nonsequential reads retain per-chunk verification and give up whole-blob EOF
verification. The native adapter still retains at most 16 resident blob readers,
with evicted in-flight readers allowed to finish.

This is an evaluated implementation, not a production migration decision. Its
decoded-cache allowance is additional to the existing shared in-flight chunk
budget. Before enabling it broadly, account for that additional per-reader
memory in the storage tuning contract and address the 65-entry cyclic regression
below. A narrower alternative is to limit decoded reuse to the FSKit evaluation
whose resident reader count is already bounded.

## Replay method

The permanent `decoded-seek-replay` benchmark uses the production packed
`ChunkSource`, a temporary local object store, identical warm compressed-cache
settings, and a 64 MiB decode budget. Disabled and enabled decoded-cache cases
run in the same test executable, with deterministic shuffled order. The probe
records successful decoded chunks/bytes and time in fetching compressed frames,
waiting for decode admission, and decoding plus verification. These are elapsed
phase times including asynchronous scheduling, not exclusive CPU measurements.
Instrumentation exists only in test builds.

The launch replay uses the 30 extra ranges measured in the preceding
[FSKit investigation](../2026-09-18-native-fskit-read-ranges/README.md), retained
in `benchmarks/fixtures/native-launch-ranges.json`. The histogram did not record
request order, so replay uses ascending file offsets. It repeats that sequence
17 times in standard mode, checks every returned byte, and clamps the final
request at EOF. This isolates a seek pattern; it is not a recreation of the
concurrent FSKit request schedule.

Without `--input`, deterministic synthetic bytes make the replay portable.
With `--input`, the runner requires the original measured GNU awk executable's
SHA-256 and length. Both modes use production FastCDC chunk-size parameters for
the launch cases. Separately constructed fixtures cover:

- sequential, one-seek and two-seek activation controls;
- single-chunk sizes of 2 MiB minus one byte, exactly 2 MiB and plus one byte;
- cyclic working sets on both sides of the 2 MiB resident limit;
- cyclic working sets of 63, 64 and 65 small chunks, isolating the entry limit.

All cases check exact bytes, cache residency bounds and release of the packed
source. The tiny-decode-budget correctness test checks that cache entries do not
retain permits required for later reads. Other tests cover sequential integrity,
EOF, cross-chunk reads, failure handling, LRU eviction and source-owner release.

## Replay results

Three repetitions using the measured GNU awk binary, median milliseconds:

| Case | Cache disabled | Cache enabled | Completed decodes, disabled / enabled |
| --- | ---: | ---: | ---: |
| Launch ranges, 17 cycles | 46.148 | 3.696 | 153 / 7 |
| Sequential | 1.965 | 1.972 | 4 / 4 |
| One seek | 0.556 | 0.583 | 1 / 1 |
| Two seeks | 1.079 | 1.078 | 2 / 2 |
| One chunk, 2 MiB minus 1 | 47.792 | 3.258 | 34 / 2 |
| One chunk, 2 MiB | 52.536 | 3.745 | 34 / 2 |
| One chunk, 2 MiB plus 1 | 48.884 | 48.370 | 34 / 34 |
| Working set, 2 MiB minus 1 | 26.909 | 2.132 | 138 / 9 |
| Working set, 2 MiB | 26.832 | 2.081 | 138 / 9 |
| Working set, 2 MiB plus 1 | 27.099 | 14.490 | 175 / 89 |
| 63 small chunks | 16.348 | 1.951 | 1071 / 64 |
| 64 small chunks | 16.295 | 1.959 | 1088 / 65 |
| 65 small chunks | 16.568 | 19.997 | 1105 / 768 |

Launch replay improves 12.5x, with decoded bytes falling from a median 44,285,765
to 2,015,329. Its resident cache contains the executable's four decoded chunks,
1,153,472 bytes total. Both modes issue zero physical chunk-range requests during
measurement because the compressed cache is warm. Aggregate elapsed decode and
verification time falls from 63.589 ms to 3.244 ms; compressed-frame fetching
falls from 1.916 ms to 0.194 ms. Decodes overlap through read-ahead, so summed
phase time can exceed batch wall time and must not be treated as a CPU partition.

The entry boundary exposes a regression: the cyclic 65-small-chunk case is
20.7% slower with caching, despite fewer decodes. Aggregate fetch-path time rises
from 0.599 ms to 6.234 ms. Misses after partial cache hits can rebuild compressed
read planning; the trace does not isolate that mechanism from scheduling.
This workload is retained as a permanent regression case. The byte-capacity
boundary also loses much of its benefit once the working set exceeds the limit,
and an oversized single chunk correctly bypasses retention altogether.

All 78 real-input replay cases and all 26 synthetic smoke cases passed. The 90
targeted Rust storage tests passed, including the tiny decode-budget test; that
test also passed after the final fixture correction. The Python suite,
`benchmark all` and revision-contract tests pass (26 tests).

## Mounted launch results

The complete retry passed all 90 trials and 2,052 checked application executions,
including mounted correctness checks, reader release and cleanup. Median first
batch times over three repetitions, milliseconds:

| Pattern | Processes | Native before | Native after | fuser after | Host after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Shared | 1 | 360.151 | 356.542 | 987.833 | 237.636 |
| Shared | 8 | 594.546 | 396.286 | 1113.427 | 240.842 |
| Shared | 15 | 828.605 | 425.354 | 1095.686 | 269.440 |
| Shared | 16 | 875.823 | 423.504 | 1076.320 | 270.326 |
| Shared | 17 | 903.279 | 434.440 | 1117.102 | 278.997 |
| Distinct | 1 | 351.309 | 364.376 | 1023.941 | 233.308 |
| Distinct | 8 | 1702.531 | 1698.988 | 7945.873 | 1180.333 |
| Distinct | 15 | 3003.091 | 2965.738 | 14474.111 | 2110.320 |
| Distinct | 16 | 3205.293 | 3206.364 | 15268.133 | 2249.578 |
| Distinct | 17 | 3659.047 | 3546.848 | 16790.218 | 2386.402 |

Shared 17-process startup improves 51.9%, or 2.08x. Immediate repeats remain
similar: native 91.497 ms before versus 92.221 ms after at that width. Distinct
paths show little change, consistent with reuse within each reader rather than
a shared cache across executable file identities. These are batch wall times,
not per-process latency. Libraries remain on the host.

The baseline is the preceding `workloads-read-control.json`; both use tracing
disabled, identical tool bytes, input and snapshot. Source receipts differ only
in the chunk reader, test-only packed-fetch instrumentation and replay probe.
This is a before/after comparison on a shared Mac, not a randomized same-binary
cache toggle. The direct replay supplies that same-binary control. Production
fuser still opens a reader for each callback; its one-shot seeks do not activate
the new decoded cache.

The first mounted run, `workloads-decoded-cache.json`, is incomplete. It passed
54 trials, then a fuser 17-process distinct-file batch exceeded the unchanged
120-second child timeout. Cleanup passed. Final fuser counters recorded 83.1 s
in blob opens and 45.9 s in directory reads. Other Casita CI tests were running
on the shared Mac, and the preceding 15-file fuser batch had already slowed to
61.6 s. This suggests interference but does not establish the cause of the
timeout. The incomplete run is retained rather than included in complete-run
timing summaries. `workloads-decoded-cache-retry.json` repeats the full matrix
with the same binaries, source, fixture and timeout.

## Reproduction commands

`decoded-seek-replay` is registered in `benchmarks/manifest.json`, `benchmark all`
and revision comparisons. Smoke keeps every boundary with three replay cycles;
standard uses 17. Both run one sequence for the activation and sequential controls.

```sh
python3 -m benchmarks.suites.decoded_seek_replay --profile standard \
  --repetitions 3 --output results/decoded-seek-synthetic.json
python3 -m benchmarks.suites.decoded_seek_replay --profile standard \
  --repetitions 3 \
  --input /nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk \
  --output results/decoded-seek-awk.json
```

`--probe-binary PATH --no-build` reuses a built library test executable. Reports
retain its hash, source and fixture hashes, input receipts, chunk sizes, ranges,
phase measurements and correctness/release gates.

For the mounted matrix, use the tool environment variables documented in the
[real-tool report](../2026-09-18-native-fskit-workloads/README.md), then run:

```sh
python3 -m benchmarks.suites.native_fskit_workloads --profile standard \
  --repetitions 3 --metadata-files 0 --timeout-seconds 2400 \
  --output results/workloads-decoded-cache.json
python3 benchmarks/reports/2026-09-18-decoded-seek-replay/summarize.py
```

Next, prefer addressing the 65-entry cyclic regression and specifying a shared
decoded-memory budget before broad rollout. An alternative is keeping the
optimization scoped to the bounded native FSKit evaluation while testing full
mounted toolchains and real builds.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
