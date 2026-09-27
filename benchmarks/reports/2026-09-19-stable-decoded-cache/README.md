# Stable decoded-cache admission and shared memory bound

## Change

The previous 64-entry LRU regressed on a 65-chunk cyclic scan. Readers now keep
initially admitted verified chunks until reader release, without replacing them
on misses. The existing second-nonsequential-seek activation, 2 MiB per-reader
byte limit, and 64-entry limit remain.

A nonblocking, process-wide reservation limits retained decoded **payload** to
32 MiB across all repositories and readers. The reservation belongs to the
copied `Bytes` allocation and survives slicing, cache destruction, and active
reads. Exhaustion skips cache admission without waiting. Original decoded owners,
decode permits, and physical read plans are not retained by cached copies.

This is a separate bound from the store's in-flight decoding/upload budget.
`with_chunk_memory_budget_bytes` documents that distinction. The bound excludes
allocator overhead, cache metadata, compressed bytes, and ordinary in-flight
buffers. It does not enforce a total process RSS limit.

Tradeoff: cache admission favors earlier readers and earlier chunks. A reader
whose working set changes after filling its cache does not replace cold entries.
A later reader can admit data after earlier readers release their reservations.

## Permanent benchmark coverage

`benchmark decoded-seek-replay` remains registered in `benchmarks/manifest.json`,
`benchmark all`, and revision comparisons. It runs cache-off and cache-on in one
binary, randomizing configuration order with seed `0xCA517A`.

- Measured GNU awk launch ranges, sequential, one-seek, and two-seek controls.
- Single chunks and total working sets immediately below, at, and above 2 MiB.
- Cyclic scans across 63, 64, and 65 chunks.
- 15, 16, and 17 simultaneously retained readers, each with a 2 MiB chunk, around
  the shared 32 MiB cap. Readers execute round-robin, not concurrently. A separate
  Rust test exercises atomic admission from 16 concurrent threads.

Every returned byte is checked. The shared cases require exact expected retained
bytes, verify the cap, and require the shared count to return to zero on release.
Compressed data is warmed before timing. The launch range histogram supplies
ascending-offset replay order, not an observed temporal trace. Phase totals can
exceed elapsed time because read-ahead overlaps work.

Mounted trials use the existing standard native FSKit, production fuser, and host
matrix: three repetitions of shared/distinct paths at 1/8/15/16/17 workers, with
first and immediate-repeat batches. Each trial uses a fresh mount/path; this does
not flush macOS system caches. All backends use identical tool bytes and inputs.
Host dynamic libraries remain outside the mount. Rootless and teardown gates
remain enabled.

## Reproduce on the Mac

From the archive checkout, after sourcing the native evaluation environment:

```sh
cargo test --release --features cli --lib --no-run --message-format=json > results/stable-cache-build.jsonl
python3 -m benchmarks.suites.decoded_seek_replay --profile standard --repetitions 3 \
  --input /nix/store/10aai41gs426gl1dvqqnhsy6jhx27rra-gawk-5.4.1/bin/gawk \
  --probe-binary target/release/deps/casita-9ea8b8a7a48df7f2 --no-build \
  --output results/stable-cache-awk.json
python3 -m benchmarks.suites.decoded_seek_replay --profile smoke --repetitions 1 \
  --probe-binary target/release/deps/casita-9ea8b8a7a48df7f2 --no-build \
  --output results/stable-cache-synthetic.json
```

Use the actual test executable path from Cargo's JSON if its hash changes.
`measure.sh` records the exact tests and mounted command, including fixture tool
paths recovered from the prior report. Run the report validator from repository
root with `PYTHONPATH=. python3 benchmarks/reports/2026-09-19-stable-decoded-cache/summarize.py`.

## Replay results

M1 Mac, standard profile, three repetitions. Median elapsed milliseconds:

| Case | Cache off | Stable admission | Previous LRU |
| --- | ---: | ---: | ---: |
| Measured launch, 17 cycles | 44.019 | 3.476 | 3.696 |
| Sequential | 1.923 | 1.918 | 1.972 |
| One seek | 0.535 | 0.541 | 0.583 |
| Two seeks | 1.001 | 1.013 | 1.078 |
| 63 chunks | 15.415 | 1.864 | 1.951 |
| 64 chunks | 15.603 | 1.893 | 1.959 |
| 65 chunks | 15.966 | 2.142 | 19.997 |
| Working set 2 MiB + 1 byte | 26.115 | 4.813 | 14.490 |
| 15 retained readers | 726.332 | 46.664 | Not measured |
| 16 retained readers | 772.125 | 49.782 | Not measured |
| 17 retained readers | 820.596 | 98.122 | Not measured |

Cache off versus stable admission uses the same new binary. The previous LRU
column is historical, from the preceding investigation, and is not an interleaved
comparison. The 65-chunk case now completes 81 decodes instead of 1,105 without
caching, or 768 with the previous LRU. Compressed-frame fetch time falls to
0.089 ms versus the previous LRU's 6.234 ms. This is consistent with avoiding
repeated compressed-stream reconstruction; the experiment changes admission as
a whole and does not independently isolate each source of overhead.

The launch replay retains 1,153,472 bytes and performs seven decodes instead of
153, for a 12.7x elapsed improvement. Sequential and one-seek controls retain no
decoded cache bytes. Chunks larger than 2 MiB remain uncached and have comparable
elapsed time in both modes.

The shared cases retain exactly 30, 32, and 32 MiB. At 17 readers, the last reader
continues decoding rather than exceeding the cap, explaining the timing increase.
All three release the full reservation after reader teardown. This is deliberate
bounded-memory behavior, not eviction thrashing.

Validation: 92 targeted Rust tests passed, including atomic shared admission,
cloned-byte lifetime accounting, integrity, tiny decode-budget progress, and
repository protection release. The real-input matrix passed all 96 samples and
the synthetic smoke matrix passed all 32. Thirty Python harness tests passed.

## Mounted comparison

All 90 trials and 2,052 child executions passed their output and teardown gates.
Rootless mounting, native no-TCP audit, mutation/sandbox probes, publication after
cached misses, independent repositories, and release barriers passed. Cleanup
reported no errors. Source fingerprints match the checked-out implementation.
Relative to the previous cache report, source changes are limited to
`chunked_reader.rs`, its budget documentation in `chunked.rs`, and the replay probe.

Median first-batch milliseconds across three repetitions:

| Pattern / workers | Native FSKit | Production fuser | Host |
| --- | ---: | ---: | ---: |
| Shared / 1 | 348.235 | 1,267.823 | 201.543 |
| Shared / 8 | 360.310 | 1,001.022 | 217.630 |
| Shared / 15 | 425.477 | 1,274.423 | 266.239 |
| Shared / 16 | 440.378 | 1,088.267 | 232.582 |
| Shared / 17 | 384.457 | 1,095.064 | 238.319 |
| Distinct / 1 | 309.597 | 960.459 | 193.601 |
| Distinct / 8 | 1,710.104 | 8,457.543 | 1,185.096 |
| Distinct / 15 | 3,179.684 | 12,191.093 | 3,985.731 |
| Distinct / 16 | 3,372.244 | 14,078.639 | 2,619.019 |
| Distinct / 17 | 3,802.911 | 13,103.598 | 2,662.686 |

At 17 workers, the native shared-path median is 2.85x faster than fuser. Its
immediate repeat is 101.254 ms, compared with 101.409 ms for fuser and 95.249 ms
for host. Distinct-path first launch remains much more expensive than shared-path
launch, despite native being 3.45x faster than fuser in this run.

**Shared-machine noise limits cross-run conclusions.** One-minute load rose from
about 2.9 to 6.4 during the matrix. For shared/17, native individual first batches
were 365/384/493 ms, fuser 1,095/1,042/2,894 ms, and host 233/238/538 ms. The host
controls also vary materially in the distinct-path runs. Do not attribute the
new shared median's change from the previous 434 ms to stable admission alone,
or conclude that native generally outperforms the host from the distinct/15 row.
The same-binary randomized replay is the stronger evidence that the cache
regression is fixed. The mounted run establishes functional coverage and shows
that the substantial existing shared-launch benefit remains.

## Next

Prefer profiling the remaining fresh-launch work and distinct-path cost, with
quiet-machine or randomized paired runs before claiming small improvements.
Also add a phase-changing workload for long-lived readers before broadening this
cache policy: fixed admission deliberately does not adapt after filling. A
repository-scoped or configurable shared cache is an alternative if cross-repository
fairness becomes a requirement; the current process-wide cap favors a simple,
strict retained-payload bound.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
