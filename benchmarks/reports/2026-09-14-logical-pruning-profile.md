# Logical-pruning profile — 2026-09-14

**After catalog integration:** first follow the [historical source restoration](2026-09-14-pruning-source-reproduction.md), then the report-specific patch instructions below. These measurements predate that integration.

Historical source: this profile predates the retained
[individual row-ID deletion change](2026-09-14-rowid-delete.md). Apply its
[baseline restoration patch](2026-09-14-rowid-delete-baseline.patch) in a separate
checkout to reproduce the profiled metadata source from the newer implementation.

The previous GC changes are on main as `c2c6eac`. This investigation adds optional
`prune_db_*` tracing spans, without changing SQL, fence ordering or durability.
It measures the integrated packed-reader implementation, not the historical
pre-reader binaries used in earlier comparisons.

[Raw samples and provenance](2026-09-14-logical-pruning-profile.json)
contain all 18 passing samples, executable/source hashes, environment, commands,
and full phase events. Each case ran three fresh-process repetitions on local
Btrfs storage, with 128 or 1,024 initial objects and 1, 8 or 10 overlapping holds.
All cases preserve historical packs and exact reader bytes during GC, check exact
logical removals, and reclaim all pack bytes after releasing the holds.

## Results

Times below are per-case medians in milliseconds. Phase medians are calculated
independently and need not add up. The DB phases are nested inside `prune_commit`,
which is nested inside logical pruning; never add parent and child durations.

### GC while holds are live

| Objects | Holds | Whole GC | Logical prune | Retained SQL | DB commit | Checkpoint attempt |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 | 1 | 80.40 | 14.04 | 1.90 | 3.54 | 0.27 |
| 128 | 8 | 109.69 | 15.33 | 1.44 | 4.45 | 0.28 |
| 128 | 10 | 125.03 | 15.60 | 2.08 | 4.56 | 0.26 |
| 1024 | 1 | 99.94 | 37.15 | 19.96 | 7.30 | 0.79 |
| 1024 | 8 | 135.43 | 22.07 | 9.35 | 5.09 | 0.50 |
| 1024 | 10 | 197.78 | 22.10 | 9.92 | 4.14 | 0.47 |

### GC after releasing holds

Only one logical object remains to delete in this second collection.

| Initial objects | Initial holds | Logical prune | Retained SQL | DB commit | Checkpoint attempt |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 128 | 1 | 11.67 | 0.11 | 1.98 | 3.60 |
| 128 | 8 | 15.71 | 0.12 | 2.36 | 7.73 |
| 128 | 10 | 14.57 | 0.12 | 2.53 | 4.47 |
| 1024 | 1 | 29.19 | 0.12 | 4.76 | 11.48 |
| 1024 | 8 | 18.98 | 0.12 | 3.03 | 8.74 |
| 1024 | 10 | 23.87 | 0.09 | 4.36 | 9.95 |

## Interpretation and next work

For held collections at 1,024 objects, retained-set SQL takes 9.35–19.96 ms,
versus 4.14–7.30 ms for transaction commit and 0.47–0.79 ms for the checkpoint
attempt. At 128 objects SQL takes 1.44–2.08 ms. Writer admission into the metadata
connection, transaction begin, revision preparation and state update each have
case medians below 0.11 ms; validation stays below 0.32 ms. Fence admission and
release remain material (case medians 2.56–4.79 ms each).

After holds are released, retained-set SQL falls to 0.09–0.13 ms. Checkpoint
attempts then take 3.60–11.48 ms. This is a separate cost from the large first
collection's retained-set work. A checkpoint event measures the existing
best-effort attempt: errors and the returned busy status are not surfaced by the
existing caller, so a short attempt does not establish successful truncation.

The preferred next experiment is bounded batched deletion in the inline retained
path. Code inspection shows one prepared DELETE execution per stale object.
The current timer combines retained enumeration, validation, object scanning,
deletion and ingest-cache cleanup, so it does not prove individual deletes cause
all this time. Compare against the current implementation before adopting a
change, retaining malformed-set, graph/root and rollback correctness tests.
Another option is investigating post-release checkpoint behavior, but this profile
does not justify weakening online holds, fences or durability.

This is an attribution study, not a before/after performance claim. Three
repetitions and other host workloads limit precision; the 1,024-object one-hold
case was notably variable. No 20% worst-case overhead bound is certified. These
fixtures only exercise the inline SQL path, below the 65,536-object cutoff. The
permanent `metadata-collection` suite already covers both 65,536 and 65,537; any
follow-up optimization must retain that threshold coverage.

## Reproduction and validation

Use the repository development shell and the current profiling source:

```console
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# Copy the lib-test executable printed by Cargo to a stable path, then:
python3 -m benchmarks.suites.held_catalog_gc --counts 128,1024 --holds 1,8,10 --repetitions 3 --probe-binary /PATH/TO/casita-lib-test --no-build --output results.json
```

The suite is permanently registered in `benchmarks/manifest.json` and included in
`benchmark all`. No separate probe is required. The runner now hashes metadata,
SQLite and publication sources as well as payload sources. Optional tracing
captures writer-lock/blocking-pool wait, transaction begin, preparation, retained
SQL, state update, durable commit and checkpoint attempt.

Validation: 45 release collection tests, 23 release SQLite metadata tests,
288 Python benchmark tests, all-feature/all-target Clippy with warnings denied,
formatting and whitespace checks passed. All 18 measured fixtures passed their
correctness gates. No own compilation or test run overlapped measured samples.
