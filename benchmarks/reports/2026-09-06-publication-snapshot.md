# Publication snapshot profile — 2026-09-06

The primary full-index clone is **not the main publication bottleneck** in the
retained-history workload. Keep the profiling support; the measured cost does
not justify redesigning snapshot ownership or publication concurrency.

## Measurements

The existing `history-scale` fixture was run in three fresh processes at 100,
1,000 and 10,000 retained roots. Each checkpoint measures 100 individual updates
after setup in batches of 64 roots. Every update adds a deterministic 256-byte
blob and directory, then publishes a named root. Audits and reopen checks remain
outside the timed window.

| Retained roots | Update median | Publish phase median | Primary clone median | Copied payload lower bound | Clone share of total update time |
|---|---:|---:|---:|---:|---:|
| 100 | 2.511 ms | 2.318 ms | 0.00475 ms | 0.0195 MiB | 0.16% |
| 1,000 | 3.460 ms | 3.180 ms | 0.02731 ms | 0.2908 MiB | 0.86% |
| 10,000 | 22.309 ms | 21.770 ms | 0.50698 ms | 2.9220 MiB | 1.45% |

Time and payload columns are medians of the three processes' per-update
medians. The share column is the median of each process's summed clone time
divided by summed update time, including slow updates. It is therefore not the
ratio of the table's median times. At 10,000 roots the clone is about 2.3% of a
typical update and about 1.5% of total measured work.

There is **one primary snapshot per measured update**, across all 900 updates.
Memory accounting takes a median 2.4 microseconds at 10,000 roots, is excluded
from clone timing, and is included in total update time. These bytes are a
lower bound on newly copied owned payload, not an allocator profile or RSS:
spare capacity, hash-table control data, alignment overhead outside stored
elements and allocator bookkeeping are excluded.

The three 10,000-root update medians were 21.025 / 37.245 / 22.309 ms in
repetition order; clone medians were 0.516 / 0.507 / 0.503 ms. The shared host
varied substantially between runs. These measurements are phase attribution,
not a before/after speedup claim against the earlier approximately 27 ms result.

## What is measured

The local Turso fixture uses standalone `publish_current_index` during payload
flush. Coordinated state backends use `prepare_state_catalog`. Both call the
same private snapshot helper. In ordinary builds it performs the existing
clone. Test builds also record calls, clone time, copied live payload and
accounting time. The subsequent phase investigation corrected the original
description of which path this fixture uses; the clone measurements are unchanged.

Timing begins after acquiring the index/mutation locks and copying the lazy
catalog overlay. It ends when the primary `Index` clone returns. Witness copies,
lazy-overlay copies, lock acquisition, extra background-rebase snapshots and
snapshot destruction are outside this metric. This result does not establish
that all catalog copying or allocation is cheap.

The benchmark emits four extra fields on each update:

- `catalog_snapshot_calls`
- `catalog_snapshot_nanos`
- `catalog_snapshot_payload_bytes_lower_bound`
- `catalog_snapshot_accounting_nanos`

The Python reader accepts older unprofiled probes. If any snapshot field is
present, all four must be nonnegative integers on every update in that sample.
Normal library builds have no new counters or memory-accounting traversal.

## Decision and next measurement

No snapshot ownership change was made. The clone allocates and copies data,
but removing this primary copy alone would recover little of the measured
runtime. A shared or partial snapshot would have to preserve atomic mutation
capture, concurrent updates, background rebase, publication retries and crash
recovery; this workload does not justify that additional complexity.

The next useful split is **catalog preparation/building versus atomic state
commit** within the roughly 22 ms publish phase. The current probe locates most
of the remaining time there; it does not yet establish which subphase dominates.
Larger indexes, long sequences of individual publications and contended writers
remain separate workloads, so these results should not be extrapolated to them.

## Reproduction and validation

```sh
cargo test --release --lib --features s3,ssh,experimental --no-run --locked \
  --message-format=json

benchmark run history-scale --no-build --probe-binary /path/to/test-executable \
  --generations 100,1000,10000 --window 100 --repetitions 3 \
  --output benchmarks/results/publication-snapshot.json
```

The probe was built from `947f2f69081b297e86bf03d237dbfe22a59c9344` plus
the profiling changes. Its SHA-256 is
`bcd1f9c70aa04e8a90f74cd44946eceff035b66d7d986dbdb3c138b08ce42d3d`.
The discarded adaptive cache policy is absent. Builds ran outside measurements.

All **18 result samples** and **900 timed publication updates** passed validation.
The existing probe verifies retained roots and exact payloads, clean fsck, and
reopen behavior. **481 all-feature library tests passed** (17 ignored), including
publication concurrency and crash tests; **143 Python tests passed**.
All-feature/all-target Clippy with warnings denied passed.

The [numerical receipt](2026-09-06-publication-snapshot.json) retains each timed
update, all per-process medians, environment metadata, configuration, and
source/binary/result hashes. Raw output, commands, the probe binary and source
snapshots remain under `benchmarks/results/2026-09-06-publication-snapshot/`.
