# Avoid generation reads on complete NAR cache hits

The optimization removes the extra generation lookup from complete `ensure_nar`
cache hits. These hits return existing facts without publishing an association,
so they need no merge generation. The cross-handle durable generation remains the
fence for every association merge and physical-audit restore.

The important correctness detail is a **partial hit**. Its cached facts can be
invalidated after the initial lookup but before generation capture. Reusing that
old snapshot with the new generation would incorrectly republish retired facts.
The optimized path captures the generation, then reloads any cached facts it will
reuse. A later invalidation is still rejected transactionally by `merge`.

An initially empty lookup has no facts to reuse, so it needs no reload. Explicit
scrubs skip the preliminary cache lookup and acquire their comparison facts only
after generation capture. Verification-facts lookups, excluding the unchanged
availability checks, are therefore:

| Operation | Before | After |
| --- | ---: | ---: |
| Complete hit | generation + facts | facts |
| Empty miss | generation + facts | facts + generation |
| Partial hit | generation + facts | facts + generation + facts reload |
| Explicit scrub | generation + facts | generation + facts |

Partial enrichment deliberately pays one additional association read. It is not
separately timed in this experiment. Raw intake and full physical audit orchestration
retain their existing generation captures.

## Correctness gates

The new local regression opens two independent repository handles and interleaves
invalidation after either the initial association read or the facts reload. An
unrequested SHA-512 fact acts as a canary: an invalidation before generation
capture must not allow that retired fact to return. An invalidation after
capture/reload must reject the association merge. The same fixture directly
requires zero generation reads on a complete cache hit.

All 44 NAR tests pass, including the existing cross-handle generation regression.
All-features/all-targets Clippy with warnings denied, formatting, 13 Python harness
tests, and whitespace checks also pass.

## Paired release measurements

Baseline: `1355e75f34c4ee06d4ce25aebe2b0732e062a5ce`, including the durable-generation
correctness fix. Candidate: that revision plus [optimization.patch](optimization.patch).
The baseline executable was copied before modifying production source; the
candidate was rebuilt and copied afterwards. SHA-256 identities differ, and all
393 dependency artifact configurations match. Both use the identical retained
[benchmark.rs](benchmark.rs), fixtures, lockfile, Rust 1.96.0, default/native
features, and release optimization level 3.

Four repetitions per variant alternated AB/BA order. Each case used 20 Criterion
samples, a 200 ms warmup and a requested 2 s measurement interval. Both executables
ran on logical CPUs 4 and 8 of the same Ryzen 7 7840S host with local Btrfs storage.
The host and cores were shared with other work. Neither our builds nor our test
suites ran during measurement.

All eleven cases passed standalone correctness checks in both executables and
retained the gates during timing: exact canonical digest and size, expected
encoding/hash work, cache-hit statistics, and independent stored-content scrubs.

Values below are medians of four process medians. Negative changes mean faster.

| Case | Before | After | Change |
| --- | ---: | ---: | ---: |
| Local cached, 1 file | 200.49 µs | 175.16 µs | -12.6% |
| Local cached, 256 files | 322.75 µs | 215.92 µs | -33.1% |
| Second handle cached, 1 file | 249.73 µs | 163.79 µs | -34.4% |
| Second handle cached, 256 files | 286.42 µs | 219.94 µs | -23.2% |
| Local scrub, 1 file | 4.73 ms | 5.23 ms | +10.6% |
| Local scrub, 256 files | 30.17 ms | 25.58 ms | -15.2% |
| Local repeated intake, 1 file | 15.64 ms | 29.67 ms | +89.7% |
| Local repeated intake, 256 files | 50.81 ms | 45.41 ms | -10.6% |
| Memory cached, 1 file | 6.44 µs | 6.42 µs | -0.4% |
| Memory scrub, 1 file | 16.84 µs | 17.28 µs | +2.7% |
| Memory repeated intake, 1 file | 100.44 µs | 101.86 µs | +1.4% |

All four local cache-hit case medians fell. The one-file original-reader case
improved in each paired repetition, with the overall median moving from about
200 to 175 µs. This supports the intended work reduction; the regression test
independently proves that the extra database lookup is gone.

**The precise percentages are not isolated effect estimates.** There were large
storage-latency spikes, including baseline cached reads above 500 µs and a
baseline 256-file scrub near 100 ms. Even raw intake, whose implementation this
patch does not change, showed very large and inconsistent differences. Keep the
outliers and treat raw-intake/scrub timing changes as inconclusive. Do not claim a
90% raw-intake regression or attribute the larger cache-hit reductions entirely
to this optimization. Memory controls also varied between processes.

[results.json](results.json) retains all 88 process/case measurements, raw
iteration/time arrays, estimates, paired changes, affinity, system load, and binary
hashes. Per-process logs are retained alongside it. No samples were discarded.
[provenance.json](provenance.json) records the build inputs and procedure.

## Permanent corpus and reproduction

The `nar_generation` group in `nar_associations` now includes eleven permanent
cases: cached verification, second-handle cached verification, repeated raw intake,
and explicit scrubs. Local cases use 1 and 256 files of 1,024 bytes; memory controls
use one file. The expanded cases are registered in `benchmarks/manifest.json` and
run in `benchmark all --suites core-primitives`.

Use the [benchmark guide](../../nar-generation.md) for timing boundaries and build
commands. Copy this report's retained `benchmark.rs` into the baseline and candidate
checkouts. Copy the earlier report's [dependencies.lock](../2026-09-22-nar-generation/dependencies.lock)
to each checkout's `Cargo.lock`, which is ignored by Git. Use `--locked` and separate
Cargo build directories for separate worktrees, or build sequentially in one
worktree and retain the baseline executable before editing the source. Apply
`optimization.patch` only to the candidate with
`git apply --unidiff-zero /path/to/optimization.patch`.

```sh
cargo bench --locked --bench nar_associations -- nar_generation --test
taskset -c 4,8 python3 benchmarks/tools/compare_nar_generation.py \
  --baseline-binary /path/to/baseline-binary \
  --candidate-binary /path/to/candidate-binary \
  --repetitions 4 --output /tmp/new-nar-cache-hit-comparison
```

The runner rejects identical executables and missing or extra cases. The earlier
[eight-case report](../2026-09-22-nar-generation/README.md) retains its own benchmark
source and uses `--case-set original` when reproduced with the current runner.
