# Publication phases and snapshot release — 2026-09-06

Releasing the validation snapshot before the atomic state commit reduces the
10,000-root update median from **22.19 ms to 4.33 ms (80.5%)** in the paired
retained-history benchmark. The expected revision is captured first and still
checked by the state store. No durability or checkpoint setting was changed.

## Finding and change

The local fixture publishes a standalone payload catalog during `flush`, then
commits records and roots through Turso. It does not use the coordinated
`prepare_state_catalog` path. The earlier snapshot report's path description
has been corrected; its clone timings remain valid because both paths were
instrumented.

The phase probe showed that state commit dominates at 10,000 roots. In the
paired baseline, it consumes about **78% of total publication time**. Catalog
building itself takes a median **0.13 ms**; the broader payload-preparation
phase takes **3.32 ms**.

`publish_inner` previously retained its read snapshot until it returned from
the write. It now captures `snapshot.revision()`, releases the snapshot, and
commits against that same revision. An old read transaction can constrain WAL
checkpointing and restart. Releasing this unnecessary reader removes that
constraint while preserving the revision check that protects validation.

The measured reduction in commit time and file growth is consistent with that
WAL explanation. This experiment does not separately count internal checkpoint
attempts or attribute their exact engine-level costs. Other callers may still
hold legitimate read snapshots, so these results do not establish performance
under long-lived external readers.

## Paired measurements

Three fresh-process repetitions use the same deterministic fixture at each
scale, with before/after ordering alternating by repetition. Setup uses
64-root batches, followed by 100 individual updates per checkpoint. Each update
adds a 256-byte blob and a directory, then publishes a root. Rows show medians
of the three processes' per-update medians.

| Retained roots | Update before | Update after | Change | State phase before | State phase after |
|---|---:|---:|---:|---:|---:|
| 100 | 2.575 ms | 2.657 ms | +3.2% | 0.184 ms | 0.201 ms |
| 1,000 | 3.350 ms | 3.560 ms | +6.3% | 0.219 ms | 0.267 ms |
| 10,000 | 22.190 ms | 4.325 ms | **−80.5%** | 17.017 ms | 0.316 ms |

The smaller fixtures were **0.08–0.21 ms slower** in these runs. This is a
shared-host experiment, not a claim that every workload improves. The new
state phase includes explicit snapshot release; previously destruction happened
outside that phase. Total update/publication timing includes destruction in
both variants, so the overall improvement is not a timing-boundary change.

At 10,000 roots the three before/after update medians were:

| Repetition | Before | After | Order |
|---|---:|---:|---|
| 1 | 22.075 ms | 4.166 ms | Before, after |
| 2 | 22.190 ms | 4.325 ms | After, before |
| 3 | 23.094 ms | 11.272 ms | Before, after |

Every pair improved, although the third candidate run was substantially slower
than its first two runs. Raw repetitions and per-update measurements are
retained rather than removing the slower run.

The measured repository file lengths at 10,000 roots fall from **90.74 MiB to
15.51 MiB** in every pair. These totals include database, WAL and payload files;
they are not an isolated WAL measurement or allocated disk blocks. Filesystem
compression and allocation metadata are outside this metric.

After the change, payload preparation accounts for about **76% of publication
time**. Its median is 3.05 ms, including pack flushing, catalog preparation and
publication; nested catalog building is only 0.11 ms. Further work should split
the remaining preparation costs before changing their durability behavior.
Including slow updates, catalog building accounts for about 3.5% of preparation
time in the candidate, using the median of each process's summed-time ratio.

## Profiling and correctness

The test-only per-repository profile records six disjoint phases:
coordination wait, snapshot read, validation/mutation assembly, payload
preparation, state commit, and catalog finalization. Call counts preserve retry
information. Each phase timer ends without retaining its profiling mutex
across an await. Finalization is not called on this standalone-catalog fixture.

Catalog-building timers are nested inside preparation and must not be added to
the six disjoint phases. The reader checks complete nonnegative integer
counters, records the backend coordination mode, and rejects phase totals
larger than the enclosing publish time. Normal builds have none of these new
profiling fields, timers or mutexes.

The regression tests require the validation snapshot to be released when the
backend receives a commit, including after a forced revision race. They also
check that exact-revision publication fails stale without publishing objects or
roots, and that independently retained reader snapshots keep their old view.
Existing revision checks, closure verification and mutation-session protection
remain in place.

The paired comparison validates **36 result samples and 1,800 timed updates**,
including exact retained roots and payloads, clean fsck and reopen checks.
Every update has one snapshot/build and one call to each applicable publication
phase. The initial discovery run contains another 18 samples and 900 updates;
it is kept separately from the paired results.

## Reproduction and receipts

Both binaries use the same profiling harness and locked dependencies, built
with `--release --lib --features s3,ssh,experimental`. The baseline is
`947f2f69081b297e86bf03d237dbfe22a59c9344` plus profiling. The candidate adds
snapshot release before the state commit.

- Before probe SHA-256: `b06564af75546963bb0fdc8c10de3bd0ea8d6c8d3e9a53faa072eb40b8a06ffe`
- After probe SHA-256: `f7eb31f04769844939889a18bfe5f59050bdeffe4ec6413b94ab4fb35dd26313`

```sh
benchmark run history-scale --no-build --probe-binary /path/to/probe \
  --generations 100,1000,10000 --window 100 --repetitions 1 \
  --output benchmarks/results/publication-phases.json
```

Run each binary three times, alternating order. Builds were outside timed
measurements. The [numerical receipt](2026-09-06-publication-phases.json) contains
all paired updates, per-process medians, phase shares, environment metadata,
configuration and source/binary/result hashes. Full raw outputs, commands and
source snapshots are under `benchmarks/results/2026-09-06-publication-phases/`.

Validation: **481 all-feature library tests passed** (17 ignored), **42
all-feature integration tests passed**, and **144 Python tests passed**.
All-feature/all-target Clippy with warnings denied, formatting and
`git diff --check` passed. The numerical receipt includes check-log hashes and
the regression test's source hash.
