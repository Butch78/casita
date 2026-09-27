# Small-chunk compression scheduling

Keep production uploads on the blocking pool. Plain inline compression reduces
handoff overhead, but can keep a ready compression batch inside one Tokio poll
and delay unrelated ready work. Yielding before chunks up to 4 KiB reduces those
bursts, but these measurements do not establish a consistent improvement across
concurrent workloads. No production compression policy was changed.

## Experiment

The permanent `compression_handoff` benchmark uses the actual codec source from
`src/compression.rs`, including its worker-local reusable Zstd contexts. Every
batch owns 64 distinct deterministic chunks. Input copies and output validation
are outside the measured compression interval. Both strategies are warmed before
paired diagnostics; execution order reverses on alternate repetitions.

Cases cover random bytes and structured text at 1,024, 4,095, 4,096, 4,097,
16,384, and 65,536 bytes, with concurrency 1 and 16. They run on a current-thread
Tokio runtime and a four-worker runtime. Three policies are retained:

- `blocking`: always use `spawn_blocking`, matching the current upload stage.
- `inline`: compress every chunk in the polling task.
- `inline_4k_yield`: yield before inline compression at or below 4,096 bytes;
  larger chunks use the same blocking branch as the baseline.

Each configuration has four diagnostic pairs with an unrelated, continuously
ready task that repeatedly yields. Diagnostics record batch completion time,
longest compression-task poll, and the ready task's longest scheduling gap.
The ordinary Criterion cases measure batch completion without that competing
task. This investigation ran their `--test` gates, not a full Criterion timing
study. Diagnostic timings include dispatch and joining the compression job.

Every batch must return each indexed chunk exactly once, declare the correct
Zstd frame length, and decompress byte-for-byte to its original contents.

## Observations and limits

The initial [plain comparison](plain.json) demonstrates the fairness tradeoff.
For serial batches on the current-thread runtime, median diagnostic values were:

| Chunks | Blocking batch | Inline batch | Blocking ready-task gap | Inline ready-task gap |
|---|---:|---:|---:|---:|
| 64 × 1 KiB random | 959 µs | 281 µs | 28 µs | 281 µs |
| 64 × 4 KiB text | 2,241 µs | 477 µs | 21 µs | 477 µs |

The inline batch can finish sooner while the other task waits much longer.
This is a codec microbatch result, not measured application latency: actual
imports also perform storage operations and other awaits.

The [cooperative comparison](cooperative.json) retains all three policies.
The yielding variant shortened polling bursts, but some concurrent cases lost
batch throughput. Host noise also limits interpretation: at 4,097 bytes, where
the cooperative variant and baseline both execute the blocking branch, paired
median completion ratios ranged from 0.89 to 1.28. These controls prevent treating
similar-sized changes in other cases as reliable gains or regressions.

The cooperative run recorded approximately 40% external CPU use and competing
compiler processes. Both reports are explicitly marked `timing_accepted: false`.
There was no quiet-host admission and no end-to-end tar speedup claim. The
evidence supports retaining the current policy while preserving this experiment
for a controlled rerun; it does not justify a new production cutoff.

## Permanent corpus and reproduction

The benchmark is registered in `benchmarks/manifest.json` and included in
`benchmark all`. Its final matrix has 144 runnable cases and covers both sides
of the proposed 4 KiB cutoff. Both experiments passed every release correctness
case: 96 in the first version and 144 in the final version. Their 960 paired
diagnostic samples and full logs are retained. The initial benchmark source is
saved as [plain-benchmark.rs.txt](plain-benchmark.rs.txt), and the cooperative
run's source as [cooperative-benchmark.rs.txt](cooperative-benchmark.rs.txt).
The permanent target is `benches/compression_handoff.rs`. Reports include
executable and source hashes.
All-features/all-targets Clippy, formatting, whitespace checks, and the seven
benchmark registration/orchestration tests pass.

```sh
devenv shell cargo bench --features experimental --bench compression_handoff --no-run
# Use the executable path printed by Cargo; this records host activity and
# validates the complete matrix while collecting paired diagnostics:
python3 benchmarks/reports/2026-09-11-compression-handoff/run.py \
  /path/to/compression_handoff-BUILD_ID /tmp/compression-handoff-new
# Full Criterion timing, including the same paired diagnostics and gates:
devenv shell cargo bench --features experimental --bench compression_handoff
```

The runner requires Linux `/proc` and a fresh output directory. Run it from
the repository's development environment. It records contention without calling
the resulting timings an accepted performance comparison. Preserve equivalent
inputs, runtime configuration, and measurements of unrelated task latency when
evaluating a future production change.
