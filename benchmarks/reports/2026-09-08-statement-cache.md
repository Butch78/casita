# Prepared-statement caching comparison — 2026-09-08

Warm metadata collection below the 65,536-object cutoff was faster in every
paired run, but this shared-host experiment does **not establish an end-to-end
speedup**. Timing variation was substantial, including on unchanged control
paths. Initial large CLI collection slowdowns did not reproduce consistently
in focused follow-ups; smaller regressions remain inconclusive.

The more useful finding is a pre-existing collection performance cliff:
retaining 65,536 leaf objects takes roughly one second, while retaining
65,537 takes tens of seconds. Both revisions show it. Profiling the larger
collection implementation is the next recommended optimization task.

## Comparison and method

Baseline: `69b025cd0696a393e38badf3a0b8a514d4252ee5`.
Candidate: `e121c660be2bc0bba5150a04d78da39cef1cfa62`.

Both detached checkouts used the same copied `Cargo.lock`, Rust 1.96.0,
`--release --locked --features cli`, and retained CLI and library-test binaries.
Builds completed before measurements. The CLI binaries contain the exact
revisions; the native probes add the identical test-only collection fixture
to each revision. Source equality was checked after removing that fixture.

The main matrix has three paired rounds, alternating baseline/candidate order
by round, with suites run serially. It covers:

- Standard small-file and mixed corpora: collection, unchanged import, and
  edits in place, with warm filesystem caches.
- A 2,048-file graph with traversal thresholds of 1,024 and 250,000 objects,
  verifying actual spill activity and the no-spill control.
- Ten retained generations with tiny changes and unchanged imports.
- History publication at 100, 1,000 and 10,000 roots, with 100 timed updates
  per checkpoint and 64-root setup batches.
- State publication with four competing writers and 300 iterations.
- Repeated metadata collection at 256 and 65,537 objects on the same writer.

The metadata fixture contains distinct verified blob records, no named roots
or links, and retains every record. It times only the collection commit,
then verifies zero removals and the exact inventory and revision after reopen.
It isolates retained-set installation, not reclamation of stale payloads.
The repository suite separately measures collection after an edited import.
The first collection is recorded separately from 50 warm iterations at 256
objects and two warm iterations at 65,537. An incomplete pilot using 12 warm
large-set iterations was stopped for runtime calibration; all its samples are
excluded and its partial log is retained.

An additional three paired rounds use exactly 65,536 objects, with the same
fixture and two warm iterations, to check the adjacent algorithm boundary.
These boundary runs occurred after the main matrix, so their size comparisons
are diagnostic rather than simultaneous paired measurements.

The machine was a 16-thread Ryzen 7 7840S, using Btrfs on encrypted local
storage and the performance governor. One-minute load averages at main-run
starts ranged from **7.2 to 88.3**. No competing jobs were stopped and no
measurements were discarded as outliers. There is no statistical significance
claim or production latency guarantee from these runs.

## Warm metadata collection

Rows show the median of each process's warm median. A negative paired change
means the candidate was faster. Individual process medians and every timed
iteration are in the numerical receipt.

| Objects | Before | After | Change of medians | Changes within the three pairs |
|---:|---:|---:|---:|---|
| 256 | 35.28 ms | 27.88 ms | −21.0% | −53.0%, −14.0%, −25.3% |
| 65,536 | 1.023 s | 0.611 s | −40.3% | −53.1%, −32.8%, −41.3% |
| 65,537 | 34.788 s | 45.169 s | +29.8% | −2.5%, +41.6%, −9.2% |

The direction below the cutoff is consistent, but the magnitude varies with
the run. Above it, the candidate is faster in two pairs and slower in one;
the change of medians alone would misleadingly suggest a stable regression.
The measurements do not isolate compilation time from other engine costs.

The [collection implementation](../../src/metadata/sqlite.rs) uses bounded
in-memory validation through 65,536 objects. Above that, it installs temporary
retained/reference tables and pages joined records. The adjacent-count results
make this algorithm switch a stronger profiling target than further statement
caching. The responsible SQL operation has not yet been isolated.

## End-to-end results and confirmation

Selected main-matrix medians are below; the receipt includes all operations,
process values, CPU usage, memory measurements, and paired differences.

| Operation | Before | After | Change of medians |
|---|---:|---:|---:|
| Small-file CLI collection | 174.49 ms | 329.76 ms | +89.0% |
| Mixed-corpus CLI collection | 115.98 ms | 107.98 ms | −6.9% |
| Forced-spill graph collection | 735.31 ms | 593.91 ms | −19.2% |
| No-spill graph collection | 52.09 ms | 113.47 ms | +117.8% |
| Ten-generation tiny-delta import | 156.85 ms | 136.28 ms | −13.1% |
| Ten-generation unchanged import | 106.42 ms | 96.25 ms | −9.6% |
| Update at 100 retained roots | 64.36 ms | 40.65 ms | −36.8% |
| Update at 1,000 retained roots | 45.39 ms | 34.18 ms | −24.7% |
| Update at 10,000 retained roots | 44.46 ms | 50.17 ms | +12.8% |
| Turso competing-writer round | 7.98 ms | 14.42 ms | +80.8% |
| Turso independent commit with retries | 9.42 ms | 13.20 ms | +40.1% |

The apparent small-file and no-spill graph collection regressions occurred in
all three initial pairs. Each received five additional focused pairs, starting
with candidate/baseline order and alternating thereafter. Controls were
unchanged import and graph verification, respectively.

| Focused confirmation | Before | After | Change of medians | Changes within the five pairs |
|---|---:|---:|---:|---|
| Small-file CLI collection | 199.15 ms | 205.15 ms | +3.0% | +19.7%, −54.1%, −9.8%, −26.6%, +19.8% |
| No-spill graph collection | 60.93 ms | 70.08 ms | +15.0% | +15.0%, +23.1%, −5.0%, +111.6%, −17.1% |

The large initial regressions did not reproduce consistently. The graph
confirmation still has a higher candidate wall-time median, so it does not
prove equivalence. Its median CPU time is 50 ms in both variants; small-file
collection CPU medians are 120 ms and 130 ms. These CPU measurements have
10 ms reporting precision. A quieter, isolated run is needed to settle small
end-to-end differences.

Imports, history updates, and empty publication contention do not execute the
changed collection queries. Their sometimes large movements are controls for
confounding effects, not evidence of benefits from this commit. In the
candidate history runs, payload preparation accounts for a median **61.2%**
of total timed update duration at 10,000 roots; state commit accounts for
**27.9%**. Those shares use summed per-update timings within each process,
then the median across processes. Payload preparation remains a separate
profiling target for import/publication latency.

## Validation and receipts

All **36 main runs, six boundary probes, and 20 confirmation runs passed**:
280 result samples in total, including 1,800 timed history updates. Validation
uses the existing suite-specific root, restore, fsck, snapshot/CAS, and spill
checks, plus exact reopened inventory checks for the added metadata fixture.
No production source was changed for this experiment.

The [numerical receipt](2026-09-08-statement-cache.json) contains raw results,
all paired values, commands, artifact hashes, environment records, the
excluded pilot log, and the exact temporary probe and orchestration sources.
For suites whose environment revision describes the harness checkout, use
the explicit comparison artifact or execution revision to identify the tested
binary. The copied lockfile, binaries, build logs, and individual outputs are
retained under `benchmarks/results/2026-09-08-statement-cache/` (ignored).

Lockfile SHA-256:
`adc49216fe1078d4f74bb91ab3c59f397a5d8a22bda8b51085e1f6603c6ad05d`.

The embedded scripts record exact reproduction commands. To rerun using the
retained local binaries, enter `devenv shell`, choose new output filenames,
and use the recorded `benchmark run` commands. The native collection probe is
`metadata::sqlite::tests::benchmark_statement_cache_collection`; select object
counts and warm repetitions with `CASITA_COLLECTION_COUNTS` and
`CASITA_COLLECTION_ITERATIONS`. The scripts themselves contain local paths
and require adaptation when reproducing on another machine.
