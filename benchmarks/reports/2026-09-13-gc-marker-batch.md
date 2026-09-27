# Bounded local dead-pack marker publication

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

**Do not adopt this prototype on the available evidence.** Correctness checks pass,
but benefits are inconsistent and the ten-hold large fixture regresses in all
three whole-GC pairs. The live runtime has been restored to its pre-experiment
implementation. The permanent benchmark retains boundary cases, and the
[prototype patch](2026-09-13-gc-marker-batch-prototype.patch) retains the complete
implementation and its cancellation/partial-commit regression test.

## Results

All 36 samples (18 pairs) passed the live-GC and after-release correctness gates.
Medians in milliseconds; paired changes are medians of the three individual ratios,
not ratios of medians. Negative percentages mean the prototype is faster.

| Blobs | Holds | Whole GC before → prototype | Paired whole-GC change | Finish deletions before → prototype | Paired finish change | Paired release change |
|---:|---:|---:|---:|---:|---:|---:|
| 128 | 1 | 92.77 → 159.67 | +72.1% | 43.85 → 84.72 | +93.2% | +42.2% |
| 128 | 8 | 209.28 → 216.20 | −16.7% | 147.79 → 142.22 | −17.8% | +13.6% |
| 128 | 10 | 211.36 → 222.74 | −0.3% | 161.73 → 160.91 | −2.1% | −1.3% |
| 1,024 | 1 | 131.44 → 121.64 | −7.5% | 57.52 → 54.51 | −1.1% | −1.7% |
| 1,024 | 8 | 194.93 → 147.27 | −15.9% | 86.10 → 79.01 | −28.5% | −0.6% |
| 1,024 | 10 | 193.30 → 231.12 | +19.6% | 123.42 → 137.62 | +11.5% | +53.3% |

The large eight-hold case's whole-GC changes were −15.9%, +29.6%, −44.7%; the
large ten-hold case's were +57.6%, +3.3%, +19.6%. Eight holds look promising, but
the benefit does not carry reliably across the batch boundary. The 128/one live
GC does not use dead-marker batching at all, yet its paired whole-GC changes
were −1.9%, +72.1%, +107.7%, a strong warning about environmental variability.
The experiment cannot separate all implementation effects from that variability
and **cannot certify the 20% overhead ceiling**. A 19.6% median regression is not
evidence of a ceiling when individual pairs exceed it substantially.

The debug corpus smoke checks recorded zero, one and two group commits for
128 blobs with one, eight and ten holds respectively, confirming that the
permanent cases execute both sides of the batching boundary. Raw release samples
retain the same phase counters. No causal performance cliff is claimed from
these noisy data.

[Raw paired results](2026-09-13-gc-marker-batch.json) retain every timing and process.

## Recommendation

Keep the simpler per-marker publication path for now. Next preference: run a
narrow syscall profile of that path on a quieter host to distinguish directory
syncs, file syncs and scheduling waits. Another option is a one-marker comparison
using `LocalDurability::put`, to determine whether changing the existing local
write path helps without a new batch state. Do not add more batching machinery
until a repeatable end-to-end benefit justifies it.

## Change under test

Ordinary local collection prepares markers for wholly dead packs using the existing
`LocalDurability::prepare` path, keeping at most eight completed markers per group
plus the existing two in-flight compactions. It commits each group through
`LocalDurability::commit`, which renames the prepared files and syncs their shared
directory chains. Only after successful commit does GC remove these packs from
the in-memory index and enqueue retirement candidates. Markers keep their existing
on-disk format. Partial-live compactions, remote storage and emergency-space
collection retain their original ordering and publication paths.

Each staged pack remains in the dirty retry set until commit and index application
finish. Abandoned preparations and partial group failures therefore leave the old
index available for retry. Cancellation during a blocking commit may still publish
markers, but it cannot apply index changes; the existing local publication helper
retains write pins until that blocking operation finishes. Successfully published
markers can be replayed by the existing recovery code.

The comparison measures the complete change: switching these marker writes to the
existing local preparation/commit path **and** batching directory synchronization.
It does not isolate batching from a one-marker use of that same local helper.

## Workload and reproducibility

The permanent `held-catalog-gc` suite now includes 128/1,024 blobs and 1/8/10 holds.
For 128 blobs, eight and ten holds produce seven and nine wholly dead packs,
covering both sides of the eight-marker batch boundary. Setup, held reads, exact
logical-removal gates and after-release zero-pack-byte checks are unchanged.
Three paired repetitions alternate baseline/candidate order, using fresh repository
fixtures for every process. Outer `finish_deletions`, whole-GC and after-release
collection durations are comparable; per-pack intervals in the candidate exclude
the later `compact_pack_marker_commit` group phase and are not directly comparable.

The host is shared. Our builds and tests finish before measurement, but external
activity remains uncontrolled. Absolute timings and small differences need caution.

```console
# This final source tree contains the baseline runtime. Build and save BEFORE.
cargo test --locked --offline --release --features cli --lib --no-run -j 1
# In a separate copy of this same source, apply the retained prototype:
git apply benchmarks/reports/2026-09-13-gc-marker-batch-prototype.patch
cargo --config 'build.build-dir="/tmp/marker-prototype-build"' test --locked --offline --release --features cli --lib --no-run -j 1
# Save AFTER and run from either checkout (same permanent runner and fixture):
benchmark run held-catalog-gc --counts 128,1024 --holds 1,8,10 --repetitions 3 --baseline-binary BEFORE --probe-binary AFTER --no-build --output marker-batch.json
benchmark all --suites held-catalog-gc --repetitions 1 --bin-dir BIN_DIRECTORY --output /tmp/marker-batch-smoke
```

Keep the same Cargo.lock in both checkouts and use separate build directories to
avoid cross-checkout cache reuse. The actual baseline is the saved release binary
from the preceding compaction profile; its fixture is unchanged. Raw results retain
binary and source hashes, filesystem/environment information, every sample and
full process output. The prototype patch applies to this final source tree and changes only
`src/blob/pack.rs`; it includes its regression test. Apply it in reverse to restore
the baseline. The benchmark runner and fixture work with both binaries.

## Validation

Before restoring the runtime, the prototype passed:

- 95 pack tests, including the new abandoned-preparation and partial-commit retry
  test; 15 ignored benchmark/test probes.
- Eight local durability tests.
- 48 collection-filter tests initially passed; four remote tests could not start
  because the direct executable invocation lacked `rustfs` on PATH. Rerunning in
  the cached development shell passed all 59 remote metadata tests, including all
  four affected collection tests.
- 245 Python benchmark tests.
- All three registered debug `benchmark all` smoke cases, including the boundary.
- The release build and all 36 paired release samples.

The first debug build was externally terminated by SIGTERM; the single-job retry
passed. The release build emitted only the pre-existing `CountingCompactState`
unused-test-helper warning. Builds and correctness tests finished before the
accepted paired measurement. The raw report's candidate source hashes refer to
the retained prototype, not to the restored final runtime.

After restoration, the runtime source hash exactly matches the preceding profile,
and that profile's executable hash matches the paired baseline artifact. The
retained prototype patch applies cleanly to the final source. Formatting and
`git diff --check` pass. No commit or push was made.

### Reproducing after the later per-marker change

The later [individual-marker comparison](2026-09-13-gc-local-marker-put.md) changed
local marker publication. To reproduce this report's earlier runtime from the
current source, first apply `2026-09-13-gc-local-marker-put-baseline.patch` in an
isolated copy, then follow this report's commands and any experiment-specific patch.
