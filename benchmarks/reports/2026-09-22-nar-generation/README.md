# NAR invalidation generation performance, 2026-09-22

The durable generation has a measurable cost on the local association cache-hit
path in this run: median latency rose by **10–21%**, or **26–41 µs per call**,
across the original-reader and second-handle cases. This is consistent with the
extra verification-facts database read before the cache-hit early return.

Raw intake is inconclusive on this host. Both revisions had large disk/scheduling
outliers, and the paired differences do not establish a reliable throughput
regression or improvement. Do not interpret the intake medians below as a speedup
or a precise slowdown. In-memory differences were small relative to variation.

Keep the correctness fix. The useful next optimization target is the extra
metadata lookup on local cache hits, while retaining the transactional generation
check that prevents stale merges and stale audit restores.

## Valid comparison

* Baseline: `c1a9a771f4b94dac4361958e267a3be510b58a61`.
* Candidate: `1355e75f34c4ee06d4ce25aebe2b0732e062a5ce`.
* Identical permanent benchmark source, fixtures, and dependency lockfile.
* Rust 1.96.0, default `native` features, release optimization level 3.
* AMD Ryzen 7 7840S; local Btrfs storage; both processes restricted to logical
  CPUs 4 and 8 (different physical cores). The CPUs and host were not exclusive.
* Four repetitions per revision, alternating AB/BA order; 20 Criterion samples
  per case, 200 ms warmup, and a requested 2 s measurement period.
* Table values are medians of four process medians. Positive changes mean slower.

| Case | Before | After | Change |
| --- | ---: | ---: | ---: |
| Local cached, 1 file | 170.82 µs | 207.48 µs | +21.5% |
| Local cached, 256 files | 258.57 µs | 284.45 µs | +10.0% |
| Second handle cached, 1 file | 184.98 µs | 214.54 µs | +16.0% |
| Second handle cached, 256 files | 257.09 µs | 298.17 µs | +16.0% |
| Local repeated intake, 1 file | 21.50 ms | 19.71 ms | -8.3% |
| Local repeated intake, 256 files | 52.16 ms | 55.58 ms | +6.5% |
| Memory cached, 1 file | 6.26 µs | 6.31 µs | +0.7% |
| Memory repeated intake, 1 file | 101.56 µs | 99.29 µs | -2.2% |

The baseline one-file cached-read process medians were 165.9, 169.2, 607.5,
and 172.4 µs; candidate medians were 207.9, 206.5, 207.0, and 220.7 µs.
The baseline outlier is retained. Three of the four paired small-cache-hit
comparisons slowed down; the fourth was dominated by that baseline outlier.
These data support investigating the cache-hit cost, not a universal latency
percentage or a capacity forecast.

The 256-file raw-intake medians ranged from 49.9 to 67.8 ms in the baseline and
47.8 to 194.8 ms in the candidate. One-file raw intake also had an outlier above
70 ms in the baseline. No samples were discarded. System load before and after
every process, paired deltas, confidence intervals within each process, and raw
iteration/time arrays are retained in [comparison/results.json](comparison/results.json).

The benchmark checks canonical SHA-256, NAR size, zero encoding passes, expected
payload hashing for intake, association hits without hashing for cache reuse,
and a final independent stored-content scrub. All eight cases passed for both
binaries before timing and retained the same gates in measured runs. Handle
opening, initial publication, reader acquisition, assertions, report destruction,
and the final scrub are outside timing. Normal import publication and pin work
are included. This is a warm sequential workload, not a measurement of concurrent
invalidations, cold storage, large streaming files, or physical audit throughput.

## Build and measurement safeguards

`Cargo.lock` is ignored in this repository. The initial baseline worktree resolved
newer dependencies; that build was discarded. The measured binaries use the
retained [dependencies.lock](dependencies.lock), and all 393 dependency artifact
configurations matched in package identity, features, and profile.

Sharing `build.build-dir` across the worktrees then caused Cargo to reuse the
candidate executable for the baseline. Its timings are **not fix comparisons**.
They are retained, explicitly marked invalid, under
[same-binary-control](same-binary-control/results.json), including the
[pinned control](same-binary-control/pinned/results.json). Even these identical
executables showed large local-storage timing differences, which is another
reason not to overinterpret raw-intake percentages.

The valid baseline was rebuilt in an isolated directory. SHA-256 identities differ,
and an executable-content check confirmed that only the candidate contains the
new `nar-invalidation-generation` key. The comparison runner now rejects identical
executables before running them, with a regression test for that safeguard.
[provenance.json](provenance.json) records revisions, source/fixture/lock hashes,
toolchain, build profile, and filesystem details; executable hashes are in the
comparison results. Production code was not changed during this investigation.

## Reproduction and permanent coverage

The eight cases are part of `nar_associations`, registered as
`nar-invalidation-generation` in `benchmarks/manifest.json`, and included in
`benchmark all --suites core-primitives`. See [the benchmark guide](../../nar-generation.md)
for timing boundaries and build commands. The current corpus also includes
scrubs; the retained benchmark source reproduces the eight cases measured here.
For each historical checkout, copy the
retained [benchmark.rs](benchmark.rs) into `crates/casita/benches/nar_associations.rs`
and this report's `dependencies.lock` to `Cargo.lock`;
use `--locked` and a distinct Cargo build directory for each revision.

After retaining the two executables, reproduce the valid run with:

```sh
taskset -c 4,8 python3 benchmarks/tools/compare_nar_generation.py \
  --baseline-binary /path/to/baseline-binary \
  --candidate-binary /path/to/candidate-binary \
  --case-set original --repetitions 4 --output /tmp/new-nar-generation-comparison
```

Use available CPUs on another machine and retain their identity. The runner
records CPU affinity. It writes the full Criterion directory as well as a JSON
copy of every estimate and raw iteration/time sample; the JSON copies and process
logs are retained here without duplicating the Criterion directory tree.

Validation passed: eight correctness cases in each binary, all measured gates,
13 Python harness tests, benchmark Clippy with warnings denied, Rust formatting,
and `git diff --check`.
