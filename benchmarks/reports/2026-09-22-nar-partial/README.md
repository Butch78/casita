# Partial NAR association enrichment

This compares the cache-hit optimization with the preceding `measure()`
implementation while keeping the rest of revision `3727131` identical. Both
implementations retain the durable invalidation generation. The candidate
performs one additional facts read on a partial hit, after capturing generation,
to avoid publishing facts invalidated by another handle.

The four permanent `nar_partial` cases add canonical SHA-512 to a warm
SHA-256-only association on local and memory repositories containing either
1 or 256 files of 1,024 bytes. Every iteration imports into a fresh repository
outside timing, so later iterations cannot silently become complete cache hits.
Timing includes measurement and association merge. Setup, correctness assertions,
subsequent complete-hit verification, physical scrub, and destruction are excluded.
This describes warm enrichment of newly imported data, not a cold-disk test.

Every sample checks canonical SHA-256, size, independent SHA-512, one encoding
pass, full payload hashing, persistence into a complete association, and a physical
scrub. The corpus is registered in `benchmarks/manifest.json` and included in
`benchmark all --suites core-primitives` through `nar_associations`.

## Reproduce

Use a dedicated checkout of `3727131`, its development shell, the archived
`benchmark.rs` as `crates/casita/benches/nar_associations.rs`, and
`../2026-09-22-nar-generation/dependencies.lock` as the ignored `Cargo.lock`.
The comparison runner from this change supports `--case-set partial`.

Build the baseline by replacing only the span from `async fn measure(` up to
`fn report(` in `crates/casita/src/nar.rs` with `baseline-measure.rs` plus a newline.
Save the original file first. Build with:

```sh
cargo bench --locked --bench nar_associations --no-run --message-format=json > baseline-build.jsonl
```

Copy the `executable` reported for the `nar_associations` target to a separate
`baseline-bin` file **before** restoring the original source. Restore that source,
repeat the same build command with `candidate-build.jsonl`, and retain
`candidate-bin`. This sequential same-worktree procedure avoids cross-worktree
Cargo artifact reuse. Do not modify the source until each build finishes.

```sh
taskset -c 4,8 python3 benchmarks/tools/compare_nar_generation.py \
  --case-set partial --baseline-binary /path/to/baseline-bin \
  --candidate-binary /path/to/candidate-bin \
  --repetitions 4 --output /tmp/nar-partial-comparison
python3 -m unittest benchmarks.tests.test_nar_generation benchmarks.tests.test_all
```

Use suitable physical CPU cores for your machine. The reported run uses logical CPUs
4 and 8 (separate physical cores) on the same shared host as the previous investigation. No builds or
tests from this investigation run during timing, but unrelated host activity is
not controlled. Results are paired AB/BA runs; each case uses 10 Criterion samples
and a 500 ms measurement target. Medians across four process medians are
observations, not isolated causal estimates or confidence intervals.

## Results

Both batches passed every correctness gate: 64 retained process/case measurements
across two batches of four alternating pairs. Values below are medians of four
process medians, in milliseconds. Positive changes mean slower.

### Initial pass

| Case | Before (ms) | After (ms) | Change | Paired changes |
|---|---:|---:|---:|---|
| local/1 | 6.4652 | 7.3990 | +14.4% | -26.3%, +12.3%, +1.0%, +66.3% |
| local/256 | 37.8842 | 57.4567 | +51.7% | -8.3%, +105.7%, -5.8%, +0.2% |
| memory/1 | 0.0477 | 0.0439 | -8.0% | -13.7%, -1.5%, -13.9%, +1.0% |
| memory/256 | 3.6257 | 3.1624 | -12.8% | -38.6%, +0.2%, -4.5%, -27.1% |

### Confirmatory pass

| Case | Before (ms) | After (ms) | Change | Paired changes |
|---|---:|---:|---:|---|
| local/1 | 6.0081 | 5.4257 | -9.7% | -30.4%, -17.6%, -30.0%, +41.6% |
| local/256 | 36.8311 | 47.6950 | +29.5% | +18.1%, +67.3%, +5.2%, +5.3% |
| memory/1 | 0.0425 | 0.0645 | +51.8% | +55.6%, +93.5%, -0.2%, +12.8% |
| memory/256 | 2.9208 | 4.9067 | +68.0% | -0.2%, +125.7%, +11.3%, -1.1% |

The initial pass began under heavy unrelated host load. The confirmatory pass
used the same binaries and settings after the observed one-minute load average
fell from about 42 to 9; it was still not an isolated host. The repeat reverses
the sign of the one-file local result. Large memory-control swings and local
pair-to-pair variation prevent attributing the observed differences to the
additional facts read. In particular, the 256-file local case is slower in all
four confirmatory pairs, but its +29.5% aggregate is not a reliable estimate of
the reload cost when the corresponding memory control changes by +68.0%.

The deterministic tradeoff remains one extra association read on partial hits,
followed by traversal, hashing, and a durable merge. This experiment does not
establish that cost as negligible, nor establish a percentage regression. Keep
the correctness fence; repeat on an otherwise idle host before pursuing another
optimization. No production behavior is changed by this investigation.

`comparison/results.json` and `recheck/results.json` retain all Criterion raw
iteration/time arrays, per-process estimates, executable hashes, CPU affinity,
and system load. Logs include correctness results. Full Criterion directory
copies are redundant and are not committed. `provenance.json`, `benchmark.rs`,
and `baseline-measure.rs` capture the comparison inputs; the exact dependency
lockfile is retained in the earlier report. Both builds shared identical
non-workspace dependency artifacts.

## September 23 follow-up after the host became quieter

The user reported the host idle. Reused the exact retained binaries, CPU affinity,
benchmark cases, and correctness gates, with six alternating pairs instead of
four. All 48 process/case measurements passed. No compilation ran during timing.
The initial one-minute load average was 4.72 rather than about 42 in the first
September 22 batch. The four interval samples in `idle-20260923/host-vmstat.txt`
showed 60–74% CPU idle and 11–12% I/O wait, so this was a quieter shared host,
not a fully isolated system. The first vmstat line is a since-boot average and
is excluded from those ranges.

Values are medians of six process medians; positive changes mean slower.

| Case | Before (ms) | After (ms) | Change | Paired changes |
|---|---:|---:|---:|---|
| local/1 | 2.5573 | 2.6313 | +2.9% | +5.6%, +24.7%, +9.4%, +1.7%, +13.1%, -6.8% |
| local/256 | 57.4262 | 56.8688 | -1.0% | +25.9%, +13.8%, +6.6%, -20.4%, -3.2%, -6.3% |
| memory/1 | 0.0849 | 0.0851 | +0.2% | -19.5%, +11.0%, -3.7%, +2.8%, +0.7%, -2.0% |
| memory/256 | 6.5268 | 6.3404 | -2.9% | -2.1%, +0.3%, -11.6%, -3.1%, -15.3%, +3.6% |

The one-file local case has a modest observed penalty: about 74 microseconds
(+2.9% by the median-of-process-medians summary), with five of six pairs slower.
Its paired changes still range from -6.8% to +24.7%; the +2.9% is an observation,
not a precise isolated estimate of one database read. The 256-file local result
is -1.0%, with three pairs in each direction, so no consistent slowdown is
resolved there. Memory controls are +0.2% and -2.9% in aggregate.

This follow-up does not reproduce the earlier large aggregate slowdowns. It is
consistent with a small cost for short partial hits that is harder to distinguish
from traversal and storage variability on the larger case. Keep the safety fence;
these results do not justify trading correctness for another optimization.

Reproduce with the earlier command, replacing `--repetitions 4` with
`--repetitions 6` and choosing a new output directory. The exact command is retained
as `quieter_host_timing_command` in `provenance.json`. Raw samples, all twelve
process logs, correctness logs, CPU affinity, and load averages are retained in
`idle-20260923/`. These results supersede the earlier batches for the practical
cost estimate; the earlier data remains available for comparison.
