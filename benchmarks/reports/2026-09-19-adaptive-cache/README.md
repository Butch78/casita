# Adaptive decoded reuse and native reader capacity

## Changes and bounds

The native evaluation now retains 32 inode readers instead of 16. The private
repository marker `reader-cache-capacity` accepts only 16 or 32;
`--reader-cache-capacity 16` provides a same-binary control. Reader positions are
still serialized per file, failures discard damaged streams, and flush releases
resident readers before the repository barrier. In-flight reads can outlive an
evicted slot. This raises a bounded reader limit; it does not eliminate eviction
at larger working sets.

Decoded reuse retains its 2 MiB/64-entry per-reader limits and the 32 MiB
process-wide payload budget. It now detects changed working sets from actual
nonsequential seek targets. Speculative read-ahead does not contribute. A cached
target resets the evidence. Replacement requires both:

- Revisiting a missed target in a bounded history of 64 distinct missed targets.
- Accumulated decoded sizes of missed target chunks at least as large as the
  retained set, without an intervening cached target.

On replacement, old entries are cleared and subsequent verified chunks can be
admitted. Budget reservations remain attached to live `Bytes` owners after
replacement. Reads never wait for cache space. The byte threshold avoids resetting
on a few repeated misses in an otherwise cached cyclic scan.

The bounded history is a deliberate limitation: a completely disjoint cyclic
working set of 65 unique targets can fall out of history before any target
repeats. Such a reader may retain its old cache and receive no reuse benefit.
All payload and bookkeeping limits remain bounded.

## Permanent benchmark matrix

`decoded-seek-replay` retains the prior cases and adds:

- `entries-above-repeated`: two seeks per chunk in the 65-chunk cyclic scan.
- `phase-history-below/at/above`: 63/64/65 distinct new seek targets after filling
  the cache, covering both sides of the 64-target history limit.
- `phase-demand-below/at/above`: seven/eight/nine misses to a 256 KiB chunk after
  retaining 2 MiB, covering the missed-byte threshold.

The new cases inherit byte oracles, admission-occupancy checks, memory bounds,
and release gates. They run through the existing manifest entry in `benchmark
all`. Cache-off and cache-on use one test executable in randomized order.

`native-fskit-workloads` now includes 31/32/33 workers as well as 1/8/15/16/17.
The 16-reader control uses a focused 17-worker matrix. The 32-reader run uses the
complete matrix. Both use three repetitions, shared/distinct paths, native/fuser/
host backends, first and immediate-repeat batches, and the same full 33-file
fixture. Selecting `--workload-workers` changes only the measured matrix; it
does not shrink the fixture. Native read traces retain correctness and overflow
gates. The control and new-capacity runs are sequential, not randomized paired
capacity trials, so backend counts provide stronger causal evidence than small
wall-time differences.

## Reproduce

`measure.sh` records the exact Mac commands and tool paths from the preceding
report. Build the root test executable first:

```sh
cargo test --release --features cli --lib --no-run --message-format=json > results/adaptive-build.jsonl
bash results/adaptive-measure.sh
```

The script runs targeted Rust tests, both replay profiles, native repository
unit tests, then mounted capacity-16 and capacity-32 matrices. It reuses the
verified native bundle/server for the second capacity instead of rebuilding.

## Replay validation

The initial measured-input matrix passed 156 samples, and synthetic smoke passed
52. All 93 targeted root Rust tests passed. Decoder work confirms adaptation:

| Case | Cache off, median decodes | Cache on, median decodes |
| --- | ---: | ---: |
| Measured launch | 158 | 7 |
| Changed 2 MiB working set | 140 | 17 |
| Cyclic 65 chunks | 1,105 | 81 |
| Cyclic 65 chunks, two reads each | 2,210 | 99 |
| Changed set, 63 new targets | 1,071 | 190 |
| Changed set, 64 new targets | 1,088 | 191 |
| Changed set, 65 new targets | 1,105 | 1,105 |

For the demand-byte boundary, seven misses leave the old 2 MiB cached; eight
replace it with the new 256 KiB chunk; the ninth demand reuses that chunk, keeping
the decode count at eight. The 65-target disjoint phase remains a documented
limitation rather than triggering unbounded history growth.

The Mac ran other compilation/test jobs during this first replay matrix.
Timings vary materially, so these work counts are more reliable evidence than
small timing differences against earlier reports. The original loaded-run raw
results are retained.

## Fixture retention and interrupted attempt

The first 32-reader matrix stopped in the third repetition's 33-file fuser
trial: dyld reported that the exact Nix GMP library referenced by the sort
executable no longer existed. Cleanup passed. This is consistent with collection
of an unrooted fixture dependency; it was an environment failure, not a byte
oracle or native filesystem failure. The incomplete report is retained as
`workloads-adaptive-32-gc-failure.json` and is not used as a successful matrix.

The exact awk, coreutils and gzip outputs were restored, with indirect Nix GC
roots under the checkout's `results/fixture-roots`. These roots retain their
entire dependency closures. `fixture-gc-roots.json` records the paths.
`pin-fixture-tools.py` records the restore/pin operation, and `measure.sh` now
calls it before running. Copy both scripts into the Mac checkout's `results/`
(as `adaptive-measure.sh` and `pin-fixture-tools.py`) when reproducing. The roots
remain as experiment artifacts; removing those three root links later releases
that retention. No global GC policy was changed.

The complete 32-reader matrix is rerun from fresh mounts with unchanged binaries,
source and tool bytes after restoring these dependencies. The completed
16-reader control is retained.

## Timing repeat after mounted work

The repeat (`adaptive-awk-repeat.json`) used the identical test executable and
sources, with three repetitions of all 156 real-input configurations. Initial
one-minute load was 2.09. All cases passed again. Median milliseconds:

| Case | Cache off | Adaptive cache |
| --- | ---: | ---: |
| Measured launch replay | 44.133 | 3.447 |
| Sequential control | 1.933 | 1.921 |
| One seek | 0.533 | 0.541 |
| Two seeks | 1.006 | 1.011 |
| Cyclic 65 chunks | 15.851 | 2.227 |
| Cyclic 65 chunks, two reads each | 50.683 | 3.255 |
| Changed set after 2 MiB - 1 cached byte | 26.881 | 3.176 |
| Changed set after 2 MiB cached | 27.000 | 3.406 |
| Changed set after oversized uncached chunk | 27.059 | 1.859 |
| Changed set, 63 targets | 15.334 | 3.657 |
| Changed set, 64 targets | 15.518 | 3.695 |
| Changed set, 65 targets | 15.721 | 16.050 |

The exact-capacity phase case performs 17 decodes instead of 139. The historical
stable-admission report measured 25.761 ms with caching enabled for that case;
that is a separate-run comparison. The current same-binary cache-off/on result
is the controlled timing comparison. The 65-target disjoint phase receives no
reuse benefit and incurs a small bookkeeping cost, as its unchanged decode count
predicts. No claim is made that this policy is optimal for arbitrary access
patterns.

## Completed mounted comparison

The retry passed all 144 trials and 5,508 child executions. Combined with the
18-trial, 612-execution control, there are 162 complete mounted trials and 6,120
checked child executions. Rootless, native no-TCP, byte/output, trace coverage,
mutation/sandbox, publication, independent-repository and release gates passed.
Cleanup reported no errors. The two completed reports have matching tool bytes,
fixture snapshot, source hashes, build identity, native binary hashes and server
hash. Bundle path spelling differs only through macOS's `/var` and `/private/var`
aliases; the validator compares artifact hashes rather than path spelling.

For 17 distinct executable paths, medians over three repetitions:

| Metric | 16 readers | 32 readers |
| --- | ---: | ---: |
| Reader opens | 39 | 17 |
| Reader evictions | 23 | 0 |
| Open service ms | 172.499 | 66.182 |
| Read service ms, including opens | 261.623 | 135.856 |
| Native first batch ms | 3,615.928 | 3,471.180 |
| fuser first batch ms | 16,648.609 | 16,982.556 |
| Host first batch ms | 2,413.968 | 2,407.835 |

The observed native end-to-end improvement is about 4%, considerably smaller
than the service-time reduction. Capacity runs were sequential and one-minute
load ranged 5.07–7.63 in the control versus 1.97–3.36 in the retry. Host medians
were similar, but this is not a randomized paired estimate of a 4% causal gain.
Open counts and eviction counts directly establish the eliminated 17-file
capacity problem.

The remaining capacity boundary is explicit:

| Distinct files | Opens | Evictions | Open ms | Native batch ms | fuser batch ms | Host batch ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 31 | 31 | 0 | 124.853 | 6,266.656 | 29,956.397 | 4,333.121 |
| 32 | 32 | 0 | 131.619 | 6,355.964 | 30,999.123 | 4,492.883 |
| 33 | 79 | 47 | 436.941 | 6,937.046 | 31,088.835 | 4,566.939 |

All nine 17/31/32-file native trials open exactly once per file with no evictions.
At 33 files, the median returns to 79 opens and 47 evictions. This change moves a
bounded capacity boundary; it does not solve arbitrarily large working sets.
At 17 shared launches the new native median is 456.656 ms, versus 1,170.106 ms
for fuser and 285.482 ms for host. The shared-path result is essentially unchanged
from the 16-reader control because it needs only one resident reader.

For the last-finishing child in each 17-file distinct batch, the new median is
3,467.725 ms total with 138.601 ms inside `Popen`; most latency remains after
process creation returns. This includes loading and executing the tool and its
stdin workload, not an isolated measurement of FSKit or executable validation.
Service times are aggregate measurements with overlap and are not an exclusive
partition of batch wall time.

## Validation and next steps

Validation totals: 93 targeted root Rust tests, one native repository test,
26 Python harness tests, 364 replay samples including the retained timing repeat,
and 162 completed mounted trials. Rust formatting and whitespace checks pass.
Run `summarize.py` and `replay_summary.py` with `PYTHONPATH=.` from repository root
to verify the saved matrices and identities.

Prefer investigating the remaining FSKit callback/launch path instead of merely
raising the reader limit again. Another concrete option is repository-scoped
reader reuse by immutable blob identity: the distinct-path fixture contains
copies of only three tool binaries. If explored, add genuinely distinct-content
executables to the permanent corpus so deduplication does not hide reader-capacity
behavior. The measured 65-target disjoint-phase limitation also remains a future
admission-policy test target.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
