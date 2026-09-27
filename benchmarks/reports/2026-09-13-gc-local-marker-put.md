# Individual local marker publication: current-host retry

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

**Keep the simpler per-marker change.** Across the main run and targeted follow-up,
39 of 42 whole-GC pairs improved. Every tested size/hold configuration improved in
median whole-GC time, and the largest median post-release slowdown was 11.6% in the
follow-up. This supports keeping the change; it does **not certify a hard 20%
worst-case ceiling**, because individual noisy samples exceeded it.

## Runtime change and safety

Both dead-pack and partial-live compaction now call one private
`put_replacement_marker` helper. Local stores use the existing
`LocalDurability::put`; other stores retain the same immutable `put_object` call.
Encoding, identities and counters are unchanged. Publication still completes before
the index changes. Emergency collection still deletes a wholly dead pack before
attempting its marker; partial-live replacement bytes remain durable before their
marker is written. There is no batching state, background queue, new public API or
on-disk format change. The common helper removes duplicated publication code.

A new regression test obstructs local marker publication with a directory at the
marker destination. It verifies that the old pack remains physically present,
the old index and dirty retry entry remain, and a retry publishes the exact marker
before applying the index change. Existing local durability tests cover the helper's
file/directory sync and pin-lifetime behavior.

## Main comparison

Five paired repetitions per configuration, 60 successful samples. Median durations
are milliseconds. Paired percentage changes are medians of individual ratios,
not ratios of the displayed medians. Negative changes mean the candidate is faster.

| Blobs | Holds | Whole GC before → after | Paired whole change | Finish deletions before → after | Paired finish change | Paired release change |
|---:|---:|---:|---:|---:|---:|---:|
| 128 | 1 | 70.43 → 53.18 | -5.0% | 24.58 → 23.34 | -13.3% | -4.3% |
| 128 | 8 | 170.49 → 95.63 | -24.7% | 127.75 → 64.67 | -37.4% | -4.6% |
| 128 | 10 | 172.19 → 153.99 | -6.6% | 130.86 → 110.44 | -17.1% | +4.8% |
| 1,024 | 1 | 98.59 → 90.42 | -3.9% | 47.73 → 40.72 | -14.9% | -4.6% |
| 1,024 | 8 | 225.54 → 168.39 | -21.9% | 149.90 → 107.24 | -25.8% | -6.1% |
| 1,024 | 10 | 242.95 → 174.75 | -23.5% | 172.15 → 117.80 | -30.2% | -4.5% |

The 1,024-blob eight- and ten-hold cases improved whole GC in all five pairs each.
The largest `finish_deletions` slowdown among all 30 primary pairs was 16.9%.
The large one-hold case had one whole-GC outlier at +67.9% and post-release GC at
+42.9%. In that pair, unchanged prune time grew from 22.83 to 56.33 ms, logical
planning from 6.65 to 16.02 ms, and finish-deletions from 52.26 to 61.10 ms. That
broad movement cautions against attributing the whole outlier to marker publication.
It is retained in every summary, not discarded.

[Primary raw results](2026-09-13-gc-local-marker-put.json).

## Follow-up and identical-binary control

A targeted follow-up repeats 128/1,024 blobs with one/ten holds three times,
24 successful samples. All 12 whole-GC pairs improve. Median whole-GC changes:
−3.2% (128/one), −14.7% (128/ten), −16.7% (1,024/one), −22.4% (1,024/ten).
Corresponding post-release changes are −4.2%, +11.6%, −8.8%, −0.9%. Individual
post-release outliers remain, including +30.8% at 128/ten.

Five identical-binary pairs at 1,024/one (10 successful samples, identical artifact
hashes) give whole-GC differences of +9.7%, +15.8%, −7.8%, −8.9%, −10.8%.
Post-release differences range from −11.7% to +4.7%. This demonstrates background
variation; it does not explain every larger outlier or establish a universal bound.
All observed configuration medians fit the 20% overhead ceiling, but the host's
individual samples do not support a hard latency guarantee.

[Follow-up](2026-09-13-gc-local-marker-put-controls.json),
[identical-binary control](2026-09-13-gc-local-marker-put-aa.json).

## Syscall profile

Two separate Linux `strace` runs use the 128/eight fixture, one for each runtime.
Both pass all live-GC and after-release correctness gates. Traces cover setup,
live GC, released GC and fixture cleanup. Summed syscall time includes concurrency
and tracing overhead, so it is neither phase wall time nor an untraced performance
comparison. The baseline's marker-directory syncs sum to 35.75 ms versus 16.10 ms
for marker-file syncs; trace lines show leaf-directory syncs before writing and
after renaming a fresh marker.

| Full-probe sync counts | Original path | LocalDurability::put |
|---|---:|---:|
| Marker file | 9 | 9 |
| Marker directories | 26 | 27 |
| Leaf directories (subset of preceding row) | 17 | 9 |
| All sync calls, including other objects and shared ancestors | 476 | 485 |

The candidate avoids repeated leaf-directory synchronization around each write,
while syncing the ancestor chain after rename. **Total sync calls increase**;
the successful untraced comparison, not the call count alone, supports the change.
The profile cannot isolate all scheduling or pin waits. Marker-directory categories
exclude ancestors outside `pack-replacements`; their leaf subset is identified from
the parent paths of synced marker temporary files. No sync lines were unparsed.

[Original trace result](2026-09-13-gc-marker-sync-profile.json),
[original derived summary](2026-09-13-gc-marker-sync-summary.json),
[candidate trace result](2026-09-13-gc-local-marker-put-traced.json).
Raw per-thread traces are retained in the corresponding `*-traces` directories.

## Reproduction and corpus

The existing permanent `held-catalog-gc` entry remains registered in the manifest
and included in `benchmark all`. Its one/eight/ten-hold cases retain coverage on
both sides of the rejected batching experiment's threshold. No new threshold is
introduced here. `--strace-dir` is an optional profiling mode of that same suite;
use a fresh directory so old per-thread files cannot contaminate a rerun.

```console
# Current tree: build and save AFTER.
cargo test --locked --offline --release --features cli --lib --no-run -j 1
# In a separate copy with the same Cargo.lock, restore the original marker path:
git apply benchmarks/reports/2026-09-13-gc-local-marker-put-baseline.patch
cargo --config 'build.build-dir="/tmp/marker-put-baseline-build"' test --locked --offline --release --features cli --lib --no-run -j 1
# Save BEFORE, then run the current permanent runner:
benchmark run held-catalog-gc --counts 128,1024 --holds 1,8,10 --repetitions 5 --baseline-binary BEFORE --probe-binary AFTER --no-build --output paired.json
benchmark run held-catalog-gc --counts 128,1024 --holds 1,10 --repetitions 3 --baseline-binary BEFORE --probe-binary AFTER --no-build --output controls.json
benchmark run held-catalog-gc --counts 1024 --holds 1 --repetitions 5 --baseline-binary BEFORE --probe-binary BEFORE --no-build --output aa.json
benchmark run held-catalog-gc --counts 128 --holds 8 --repetitions 1 --probe-binary BEFORE --no-build --strace-dir /tmp/marker-before-traces --output before-traced.json
benchmark run held-catalog-gc --counts 128 --holds 8 --repetitions 1 --probe-binary AFTER --no-build --strace-dir /tmp/marker-after-traces --output after-traced.json
benchmark all --suites held-catalog-gc --repetitions 1 --bin-dir BIN_DIRECTORY --output /tmp/marker-put-smoke
```

Use separate build directories for copied checkouts. The actual baseline is the
saved release executable from the preceding compaction profile, with the same
fixture and compiler environment. Its source matches the baseline-restoration
patch. Raw results record binary/source hashes, process output and filesystem
metadata. All accepted comparisons ran after our build and regression tests and
without strace. The host was quieter than the previous investigation but remained
shared; the raw timings retain its variation.

## Validation and next step

The release candidate passed 95 pack tests (15 ignored probes), 45 collection tests
(one ignored probe), eight durability tests, and all 94 untraced samples across the
primary run, follow-up and identical-binary control. Both traced samples passed.
The benchmark test suite has 246 passing tests, including trace accounting and
unparsed-line detection. Clippy passed for all features and targets with warnings
denied; formatting and `git diff --check` passed. The registered `benchmark all`
smoke run passed all three cases. The measured runtime matched the source before the final tracing-annotation
correction described below. Baseline restoration plus both historical experiment
patches were validated in an isolated copy. These measurements preceded commit.

Next preference: commit this small change with its benchmark evidence. No further
batching or hold-state machinery is justified by this investigation. A stricter
worst-case latency bound would require a controlled-host measurement rather than
claiming one from these noisy data.

## Final tracing correction

Before commit, the `blob.pack.compact` tracing annotation was moved from the new
marker helper back onto `compact_pack`, where it originally belonged. This changes
the general tracing span's scope, not storage ordering or the explicit collection
phase events. The benchmark subscriber filters out that general tracing target.
No timed measurements were rerun for this annotation-only correction.

The raw reports retain the original measured source hashes. For exact measured
source reconstruction, apply
`2026-09-13-gc-local-marker-put-measured-source.patch` to the final source in an
isolated copy. The baseline-restoration patch has been regenerated for the final
source; apply it independently, not after the measured-source patch.
