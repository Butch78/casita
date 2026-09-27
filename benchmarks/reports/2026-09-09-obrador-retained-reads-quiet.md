# Obrador retained-reader repeat with host activity checks

Five paired repetitions completed at 100 paths, eight readers and 200 reads per worker. All 20 accepted cases passed correctness and host-activity gates: 32,000 verified reads. Ten additional completed attempts were retained as contaminated and excluded. Original durable/process binary hashes match the [first run](2026-09-09-obrador-retained-reads.json); the API integration was not changed.

| GC | Durable median p95 (ms) | Process median p95 (ms) | Process / durable | Pairs with process slower |
|:---:|---:|---:|---:|---:|
| off | 17.676 | 20.607 | 1.166× | 4/5 |
| on | 23.750 | 21.177 | 0.892× | 1/5 |

The GC-off regression repeated in four of five pairs, with about 17% higher median p95. The GC-on regression from the first shared-host run did not repeat: process protection was faster in four of five pairs, with about 11% lower median p95. These are ratios of the per-case p95 medians, not percentiles pooled across repetitions.

| Repetition | GC | Durable p95 (ms) | Process p95 (ms) |
|---:|:---:|---:|---:|
| 1 | off | 14.211 | 20.588 |
| 1 | on | 17.236 | 21.572 |
| 2 | off | 22.188 | 20.951 |
| 2 | on | 28.102 | 20.658 |
| 3 | off | 16.714 | 20.791 |
| 3 | on | 23.750 | 20.203 |
| 4 | off | 17.676 | 20.607 |
| 4 | on | 22.544 | 22.436 |
| 5 | off | 19.468 | 20.460 |
| 5 | on | 24.988 | 21.177 |

Before every attempt, the runner required ten seconds without detected compiler/Nix/Casita benchmark or test processes and with external user-process CPU no greater than 5% of host capacity. The same conditions were checked throughout each accepted attempt. Kernel filesystem workers can execute the benchmark’s own I/O, so their CPU is recorded separately. This does not isolate hardware, eliminate external disk activity, or detect every short-lived process.

Earlier attempts could not obtain a quiet interval, were contaminated, or timed out during host activity. They are not included in the comparison. Their local logs remain under `/tmp/casita-obrador-reads-quiet-five*.work`.

```sh
python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report benchmarks/reports/2026-09-09-obrador-retained-reads.json \
  --profile standard --paths 100 --workers 8 --repetitions 5 \
  --require-quiet-host --output /tmp/casita-obrador-reads-quiet-five-userspace.json
```

See the [raw report](2026-09-09-obrador-retained-reads-quiet.json) for every accepted and contaminated read duration, activity interval, source fingerprint and binary hash. The [permanent runner](../obrador-reads.md) remains registered in `benchmarks/manifest.json` and included in `benchmark all`.
