# Individual row-ID deletion — retained, 2026-09-14

**After catalog integration:** first follow the [historical source restoration](2026-09-14-pruning-source-reproduction.md), then the report-specific patch instructions below. These measurements predate that integration.

Keep this change. Individual row-ID deletes reduce retained-set SQL time by
18–32% in the 1,024-object cases, using the same single prepared DELETE statement
shape as before. The earlier batched experiment remains rejected. The new runtime
records integer row IDs instead of owned keys during the existing object scan,
then deletes by row ID after every retained-set check has passed. All IDs remain
inside that transaction. No public API, holds, fencing or durability behavior changes.

[Initial 36 paired comparisons](2026-09-14-rowid-delete-paired.json) ·
[30 small-case follow-up pairs](2026-09-14-rowid-delete-followup.json) ·
[Combined case summary](2026-09-14-rowid-delete-summary.json) ·
[Patch to restore the measured baseline](2026-09-14-rowid-delete-baseline.patch)

## Initial comparison

Three repetitions per size/hold combination, with before/after order alternated.
Each sample uses a fresh process and local Btrfs fixture. Setup and byte/inventory
audits are outside timing. All 72 samples pass exact logical-removal, held-pack,
paused/fresh-reader and post-release pack-reclamation checks. The SQL stage is
faster in 29 of 36 pairs. This stage includes scan, validation and cache cleanup
as well as deletion; it is not a timer of DELETE execution alone.

Times are independent per-variant medians in milliseconds. Percentage changes are
medians of per-repetition `(candidate / baseline - 1)` ratios, not ratios of the
shown medians. Positive means slower. Whole GC includes physical maintenance;
release is the separate collection after holds are dropped.

| Objects | Holds | Baseline SQL ms | Row-ID SQL ms | SQL change | Whole GC change | Release GC change |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 65 | 1 | 0.648 | 0.654 | -2.6% | -2.5% | -20.1% |
| 65 | 8 | 0.863 | 0.704 | -9.5% | +15.1% | +5.4% |
| 65 | 10 | 0.721 | 0.680 | -4.7% | +3.8% | +45.4% |
| 66 | 1 | 0.761 | 0.620 | -18.2% | -35.3% | +20.1% |
| 66 | 8 | 0.801 | 0.578 | -23.1% | +7.0% | +32.6% |
| 66 | 10 | 0.937 | 0.906 | -4.7% | +2.4% | +0.0% |
| 128 | 1 | 1.864 | 1.853 | -4.9% | -2.9% | -3.2% |
| 128 | 8 | 1.377 | 1.508 | +9.5% | -1.2% | +3.4% |
| 128 | 10 | 1.193 | 1.654 | +0.4% | +6.4% | -18.0% |
| 1024 | 1 | 6.845 | 4.782 | -17.9% | -30.4% | +14.2% |
| 1024 | 8 | 5.431 | 4.370 | -31.9% | +8.7% | -0.1% |
| 1024 | 10 | 5.603 | 4.427 | -29.8% | -23.0% | -10.4% |

Small SQL cases range from improvements to roughly neutral/slower results. Whole
GC varies more than SQL, so the large whole-GC speedups above should not all be
attributed to this edit. The runtime also uses a smaller temporary stale list
(one i64 per object instead of an owned key); no RSS reduction was measured.

## Follow-up and the 20% ceiling

Initial post-release case medians at 65/10, 66/1 and 66/8 exceeded 20%. Repeat all
six 65/66-object cases with five more interleaved pairs each rather than accepting
those initial results. All 60 follow-up samples pass correctness gates.

| Objects | Holds | Follow-up whole GC | Follow-up release GC | Combined whole GC | Combined release GC |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 65 | 1 | -6.2% | -1.0% | -4.4% | -3.9% |
| 65 | 8 | +13.2% | -23.8% | +14.2% | -13.2% |
| 65 | 10 | -7.1% | -14.0% | -1.6% | +2.4% |
| 66 | 1 | -7.5% | -28.9% | -24.0% | -12.1% |
| 66 | 8 | +6.5% | -2.3% | +6.8% | +3.2% |
| 66 | 10 | -9.2% | +14.0% | -3.4% | +11.7% |

The initial large post-release regressions did not repeat. Across the combined
case medians, the largest whole-GC regression is 14.16% and the largest release
regression is 14.23%, within the 20% practical ceiling. Individual pairs still
exceed 20%; these noisy shared-host measurements do not certify a hard worst-case
bound. Pooling uses eight pairs for each 65/66 case and three for each 128/1,024
case. The follow-up was selected after observing regressions, and both complete
runs are retained rather than replacing the initial data.

## All-retained cutoff checks

The existing permanent metadata collection suite checks exact reopened inventory,
revision and zero removals at 65,536 (inline) and 65,537 (streamed) objects. Each
binary runs three processes per count; each process performs a first collection
and two warm collections. The baseline suite ran before the candidate suite, so
these are non-interleaved controls, not a strong causal performance comparison.
Warm numbers below are medians of the per-process two-operation means.

[Baseline raw controls](2026-09-14-rowid-delete-retained-baseline.json) ·
[Candidate raw controls](2026-09-14-rowid-delete-retained-candidate.json)

| Objects | Phase | Baseline ms | Candidate ms | Change |
| ---: | --- | ---: | ---: | ---: |
| 65536 | collect-first | 348.20 | 320.71 | -7.9% |
| 65536 | collect-warm | 332.62 | 293.30 | -11.8% |
| 65537 | collect-first | 778.56 | 805.62 | +3.5% |
| 65537 | collect-warm | 896.63 | 899.81 | +0.4% |

All controls pass. They reveal no concerning scan overhead at the cutoff. The
streamed algorithm is unchanged.

## Reproduction and provenance

Use separate checkouts/build directories. The reconstructed historical source is the measured
candidate. Apply `2026-09-14-rowid-delete-baseline.patch` in the baseline checkout
to restore the exact measured SQLite source, including the earlier pruning timers.
The patch removes the new regression test as well, matching the original baseline
source hash from the preceding pruning profile. Keep the same development shell
and Cargo.lock (the repository does not track Cargo.lock). Save both executables
before running comparisons; raw reports record executable and source hashes.

```console
# Candidate checkout:
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# Save the emitted lib-test executable as AFTER.

# Separate baseline checkout/build directory:
git apply /PATH/TO/2026-09-14-rowid-delete-baseline.patch
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# Save the emitted lib-test executable as BEFORE.

python3 -m benchmarks.suites.held_catalog_gc --counts 65,66,128,1024 --holds 1,8,10 --repetitions 3 --baseline-binary BEFORE --probe-binary AFTER --no-build --output paired.json
python3 -m benchmarks.suites.held_catalog_gc --counts 65,66 --holds 1,8,10 --repetitions 5 --baseline-binary BEFORE --probe-binary AFTER --no-build --output followup.json
python3 -m benchmarks.suites.metadata_collection --counts 65536,65537 --iterations 2 --repetitions 3 --probe-binary BEFORE --no-build --output retained-baseline.json
python3 -m benchmarks.suites.metadata_collection --counts 65536,65537 --iterations 2 --repetitions 3 --probe-binary AFTER --no-build --output retained-candidate.json
```

Both workload entries are already registered in `benchmarks/manifest.json` and
included in `benchmark all`. This investigation reuses those permanent probes,
including the earlier 64/65-deletion cases and the inline/streamed cutoff controls.
No own compilation or test run overlapped accepted benchmark samples.

For the earlier rejected batch experiment, first apply the baseline restoration
patch, then its prototype patch. This restores the exact measured source rather
than combining the two experiments.

## Validation and next step

53 all-feature collection tests, 24 release SQLite metadata tests, 288 Python
benchmark tests and all-feature/all-target Clippy with warnings denied passed.
The new regression test checks 0 through 129 stale objects at representative sizes,
identical native IDs across namespaces, empty IDs, rejected unknown retained keys,
unchanged state after rejection, and exact inventories/revisions after reopening.
All 132 paired GC fixtures and all 36 cutoff-control commits pass their correctness
gates. The existing validation still protects retained roots and graph links before
any deletion starts.

The candidate also passed all nine cases through the `benchmark all` smoke entry:
[completion ledger](2026-09-14-rowid-delete-all-execution.json),
[raw smoke samples](2026-09-14-rowid-delete-all-smoke.json). Formatting and whitespace
checks passed. Reproduce that entry with the directory containing the saved
`casita-lib-test` candidate executable:

```console
python3 -m benchmarks.cli all --suites held-catalog-gc --profile smoke --repetitions 1 --bin-dir /PATH/TO/CANDIDATE-BIN-DIR --output /tmp/rowid-delete-all-smoke
```

Next: commit this small change with its regression test and profiling evidence.
I would stop tuning pruning here.
