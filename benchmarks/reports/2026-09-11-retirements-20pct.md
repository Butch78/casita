# Publication retirements: 20% external CPU ceiling

The user selected a **20% ceiling** for this comparison, superseding the
previous 5% and 15% limits. Competing processes are recorded; their presence
alone does not veto a run. The gate measures total external process CPU usage.

## Matched comparison

Reuse the verified binaries from `450a633` (before) and `b279897` (after).
Both include the newer pin journal and reader inventory cache, use identical
tracked benchmark source, and were built using the same copied lockfile with
`--locked`. Binary and lockfile hashes were checked before and after this run.
Build provenance is in the [matched-build report](2026-09-10-retirements-quiet.md).

This repeats the permanent `online-holds` cases registered in
`benchmarks/manifest.json` and included in `benchmark all`: 60 and 300 imports,
16 unique 4 KiB files per import, application readers, online GC at the unchanged
5 ms interval, and CPU affinity `8,10,12,14`. No new workload, production change,
or GC scheduling option was introduced.

Before each run, require five seconds at or below 20% external CPU usage, with
a 90-second waiting limit. Monitor usage throughout execution. A timing pair
qualifies only if **both** runs stay within the ceiling in every sampled
interval. Kernel-worker activity is recorded separately because it can include
the benchmark's own I/O. Run one warmup per variant and size, then alternate
before/after order, seeking three accepted pairs with at most six attempts per
size. Retain all rejected runs.

## Results

All **22 invocations passed correctness**: sentinel reads, final checkout,
fsck, settlement of holds/claims/collector ownership/prune fences, GC phase
accounting, and exact obsolete-object removal (1,003 or 5,083 objects).

Three of three measured 60-import pairs qualified. One of six measured
300-import pairs qualified; the other five exceeded the CPU ceiling. The
runner therefore ended with an insufficient-pairs status, not a correctness
failure. The desired three-pair large-case sample was not obtained.

Only accepted pairs appear below. The 60-import values are medians of three
runs per variant; the 300-import values are a single pair, not a repeatability
estimate. Warmups are excluded.

| Imports | Accepted pairs | Metric | Before | After |
| --- | ---: | --- | ---: | ---: |
| 60 | 3 | Import seconds | 4.529 | 4.062 |
| 60 | 3 | Reader-open p99, ms | 93.57 | 75.10 |
| 60 | 3 | Objects reclaimed during imports | 969 | 969 |
| 300 | 1 | Import seconds | 42.514 | 37.755 |
| 300 | 1 | Reader-open p99, ms | 94.58 | 100.24 |
| 300 | 1 | Objects reclaimed during imports | 5,066 | 5,066 |

The median elapsed-time reduction at 60 imports is 10.3%. The individual paired
reductions were 19.3%, 19.0%, and 4.8%; a ratio of medians differs from the median
of paired ratios. The accepted large-case pair reduced import time by 11.2%,
while reader-open p99 increased by 6.0%. Reader latency did not improve in every
accepted short-case pair either.

The original roughly 5% import slowdown **did not reproduce in these accepted
pairs**. These observations do not establish a general causal speedup: the host
still permits substantial background activity, CPU gating does not isolate
I/O or cache interference, and the large-case sample is just one pair. There
is no repeated import regression here to justify optimizing the committed-
catalog checks. No further production change or profiling instrumentation was
added on the basis of this run.

## Reproduction and evidence

[Protocol and hashes](2026-09-11-retirements-20pct/protocol.json),
[summary](2026-09-11-retirements-20pct/summary.json),
[accepted/rejected pairs](2026-09-11-retirements-20pct/pairs.json),
[all raw rows and host samples](2026-09-11-retirements-20pct/runs.json.gz), and
[progress log](2026-09-11-retirements-20pct/progress.log) are retained.
The [runner](2026-09-11-retirements-20pct/run.py) and
[summarizer](2026-09-11-retirements-20pct/summarize.py) are retained alongside them.

Saved binaries and the exact lockfile remain under the local results directory:

```sh
python3 benchmarks/results/publication-retirements-cpu20-2026-09-11/run.py
python3 benchmarks/results/publication-retirements-cpu20-2026-09-11/summarize.py
```

For another invocation, first copy the runner, `bin/before`, `bin/after`, and
`Cargo.lock` to a fresh results directory so these records are preserved.
The summarizer uses only pairs explicitly accepted by the runner, rather than
pooling isolated qualifying runs from rejected pairs.
