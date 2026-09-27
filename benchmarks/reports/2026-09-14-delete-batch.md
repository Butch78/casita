# Batched logical deletion — rejected, 2026-09-14

**After catalog integration:** first follow the [historical source restoration](2026-09-14-pruning-source-reproduction.md), then the report-specific patch instructions below. These measurements predate that integration.

Later source change: [individual row-ID deletes](2026-09-14-rowid-delete.md) were
retained. To reproduce this earlier batch experiment from that source, first apply
[the baseline restoration patch](2026-09-14-rowid-delete-baseline.patch) in a
separate checkout, then apply this report's prototype patch.

Keep individual object deletes. Bounded row-ID batching made retained-set SQL
slower in **all 60 paired comparisons**. At 1,024 objects the median paired SQL
regression is 46–58%, depending on hold count. Whole-GC results are mixed, with
some case medians exceeding the user's 20% regression ceiling. The implementation
was reverted; the optional pruning timers from the preceding investigation remain.

[Full paired samples](2026-09-14-delete-batch-paired.json) ·
[Rejected prototype, including regression tests](2026-09-14-delete-batch-prototype.patch) ·
[Preceding pruning profile](2026-09-14-logical-pruning-profile.md)

## Candidate and scope

The prototype records stale integer row IDs during the existing validation scan,
then deletes batches of at most 64 IDs. Power-of-two statement widths limit the
cache to seven SQL shapes, with NULL padding for partial batches. Query-plan tests
confirm direct integer-primary-key seeks and no object-table scan per batch.
Physical row IDs stay inside the transaction that scanned them. Validation,
logical fencing, commit ordering and checkpoint behavior are unchanged.

The baseline already executes all individual deletes in one transaction. Batching
does not reduce the number of durable commits. It reduces the number of SQL
executions and the stale-list representation, but the measured retained-set stage
includes scan, validation, statement preparation, deletion and ingest-cache cleanup.
The data does not isolate which Turso operation causes the regression or establish
that row IDs alone are slower. No RSS reduction was measured.

## Paired results

Five repetitions of each count/hold combination, with execution order reversed
between repetitions. Each sample uses a fresh process and fixture. All 120 samples
pass exact logical removal, held-pack preservation, paused/fresh-reader byte checks,
and full pack reclamation after releasing holds.

SQL times are independent per-variant medians in milliseconds. Percentage changes
are medians of the five per-repetition `(candidate / baseline - 1)` ratios; they are
not ratios of the displayed medians. Positive means slower. Whole GC includes
physical maintenance; release means the separate GC after all holds are dropped.

| Objects | Holds | Baseline SQL ms | Batched SQL ms | SQL change | Whole GC change | Release GC change |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 65 | 1 | 0.664 | 1.046 | +61.4% | -3.4% | +35.9% |
| 65 | 8 | 0.791 | 1.502 | +103.5% | -4.0% | +25.0% |
| 65 | 10 | 0.704 | 3.320 | +287.0% | +10.6% | +31.5% |
| 66 | 1 | 0.737 | 1.517 | +109.9% | +45.9% | +14.6% |
| 66 | 8 | 0.741 | 1.713 | +131.3% | +8.1% | +5.7% |
| 66 | 10 | 0.803 | 1.650 | +115.1% | +1.0% | +17.7% |
| 128 | 1 | 1.040 | 1.413 | +37.1% | -12.9% | -3.1% |
| 128 | 8 | 1.109 | 1.933 | +92.0% | +6.4% | -27.8% |
| 128 | 10 | 1.124 | 2.815 | +79.6% | -10.3% | -27.1% |
| 1024 | 1 | 5.654 | 9.311 | +58.2% | +6.3% | +18.5% |
| 1024 | 8 | 5.776 | 8.844 | +55.4% | +19.9% | +32.3% |
| 1024 | 10 | 4.982 | 8.426 | +45.8% | -1.2% | +3.2% |

Whole-GC variation is substantial on this shared host: 34 of 60 pairs regress,
while 26 improve despite every SQL-stage comparison losing. This is enough to
reject the optimization; it is not a clean causal estimate of the whole-GC
regression or a certification of any worst-case bound. Case medians above 20%
include the 66-object/one-hold whole GC and several post-release collections.

## Inline/streamed cutoff control

The permanent `metadata-collection` benchmark retains every object and checks the
exact reopened inventory and revision. It covers both 65,536 (inline) and 65,537
(streamed) objects. Each binary ran one process per count, with a first commit and
one warm commit; these controls are correctness and rough scan-overhead checks,
not sufficiently repeated performance comparisons.

[Baseline control](2026-09-14-delete-batch-retained-baseline.json) ·
[Candidate control](2026-09-14-delete-batch-retained-candidate.json)

| Objects | Phase | Baseline ms | Candidate ms |
| ---: | --- | ---: | ---: |
| 65536 | collect-first | 257.71 | 269.59 |
| 65536 | collect-warm | 238.31 | 242.81 |
| 65537 | collect-first | 687.77 | 706.82 |
| 65537 | collect-warm | 818.90 | 822.86 |

## Reproduction

The final runtime is the measured baseline: `c2c6eac` plus the optional pruning
instrumentation described in the preceding report. The prototype patch applies to
that source and reconstructs the exact measured candidate SQLite source, including
its tests. Both source hashes were verified when reverting the experiment. The raw
paired report records the candidate source hashes and both executable hashes; the
preceding profile records the baseline source and executable hashes. Payload source
files match; runner changes only expand the default cases and source provenance.

In separate checkouts, build the baseline first, then apply the prototype patch to
the candidate checkout. Preserve the same Cargo.lock and development environment;
this repository does not track Cargo.lock. Use separate build directories and save
the emitted executables before switching source:

```console
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# Candidate checkout only:
git apply /PATH/TO/2026-09-14-delete-batch-prototype.patch
cargo test --locked --offline --release --features cli --lib --no-run -j 2

python3 -m benchmarks.suites.held_catalog_gc --counts 65,66,128,1024 --holds 1,8,10 --repetitions 5 --baseline-binary /PATH/TO/baseline --probe-binary /PATH/TO/candidate --no-build --output paired.json
python3 -m benchmarks.suites.metadata_collection --counts 65536,65537 --iterations 1 --repetitions 1 --probe-binary /PATH/TO/baseline --no-build --output retained-baseline.json
python3 -m benchmarks.suites.metadata_collection --counts 65536,65537 --iterations 1 --repetitions 1 --probe-binary /PATH/TO/candidate --no-build --output retained-candidate.json
```

The permanent `held-catalog-gc` entry in `benchmarks/manifest.json` now defaults to
65, 66 and 128 objects in smoke and adds 1,024 in standard. With one hold, 65 and 66
objects remove exactly 64 and 65 objects, covering both sides of the proposed batch
boundary. These cases remain in `benchmark all` even though the optimization was
rejected. The existing metadata suite retains the inline/streamed cutoff cases.

## Validation and next step

The measured prototype passed 25 SQLite tests in both debug and release, 54
all-feature collection tests, all-feature/all-target Clippy with warnings denied,
and all measured correctness gates. The patch retains query-plan checks and
regression cases for 0, 1, 2, 3, 63, 64, 65, 127, 128 and 129 stale objects,
including identical native IDs in different namespaces, empty IDs, rejected unknown
retained keys, unchanged state after rejection, and exact reopened inventories.
No own compilation or tests overlapped accepted timing runs.

After reverting the prototype, the expanded `benchmark all` smoke entry passed
all nine cases: [completion ledger](2026-09-14-delete-batch-all-execution.json),
[raw samples](2026-09-14-delete-batch-all-smoke.json). The 288 Python benchmark
tests, formatting and whitespace checks passed. Reproduce the all-entry check
using the baseline binary directory:

```console
python3 -m benchmarks.cli all --suites held-catalog-gc --profile smoke --repetitions 1 --bin-dir /PATH/TO/BASELINE-BIN-DIR --output /tmp/delete-batch-all-smoke
```

If pruning work continues, the next simpler candidate is individual row-ID deletes
without batching. That would isolate the physical-ID representation and lookup cost
without introducing multiple statement widths. It still needs measurement; this
experiment provides no evidence that it should be adopted. Keeping the existing
pruning path is also a reasonable stopping point.
