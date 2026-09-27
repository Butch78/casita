# Publication-owned retirements

Implementation against `125ae56f60d5cfc0aaa7ea3049740b43d7a9ff12`.
This follows the [design review](2026-09-09-gc-design-review.md).

## Behavior

Catalog mutations and their physical retirement candidates now share one owned
batch. Preparing a catalog takes that batch; successful publication moves its
candidates to the cleanup queue. Abort, preparation failure, and cancellation
restore the batch ahead of newer mutations. Both repository state publication
and standalone catalog pointer publication use this lifecycle. Background
rebase follow-ups remain mutation-only and cannot clone retirement ownership.

Manifest removal, pack replacement, and tombstone replacement record their
candidates under the same index lock as their catalog mutation. Cleanup can
therefore process an older committed batch while a newer writer is dirty or
prepared. The newer batch stays ineligible until its own publication succeeds.

Before deleting, cleanup checks an immutable view of the committed catalog,
then the existing online pins and deletion claims. Republishing the same
content-addressed path makes it live again. Errors and cancellation restore
unprocessed candidates to the ready queue. Orphan discovery still waits for
pending publication because it reads the mutable index. Emergency pruning
retains its existing fence and deletion contract.

The simplification removes independent retirement staging at the publication
boundary. It does not eliminate logical marking, online holds, deletion claims,
or crash recovery, and it does not change the public API or GC scheduling.

## Regression coverage

- An old prepared batch commits while a newer retirement is pending; cleanup
  deletes only the old candidate. Run with memory and durable local storage,
  with the newer batch dirty or prepared, then abort and retry it.
- Cancellation while building a catalog restores both mutation and retirement
  ownership, for state publication and standalone pointer publication.
- Republishing an identical physical pack protects it from an already queued
  retirement, with and without reopening the repository.
- Existing deletion-cancellation, late-reader, tombstone, background-rebase,
  process-crash, and emergency-prune tests remain part of the full suite.

## Benchmark protocol

Reuse the permanent `online-holds` corpus registered in
`benchmarks/manifest.json` and included in `benchmark all`: 60 and 300 imports,
16 unique 4 KiB files per import, application readers, concurrent GC, fixed
5 ms GC delay. No scheduling sweep is part of this change.

Three alternating before/after pairs per size follow one warmup per variant.
Both binaries run on CPUs `8,10,12,14`. The baseline is the saved `125ae56`
binary from the paused scheduling investigation: its harness makes the delay
configurable, explicitly set here to the same 5 ms used by the current harness.
There are no baseline production changes. Binary hashes, exact commands, source
patch, raw samples, and per-second competing-process records are retained in
`benchmarks/results/publication-retirements-2026-09-09/`.

Every run checks sentinel reads, final checkout, fsck, pin/claim/collector
cleanup, GC phase accounting, and exact obsolete-object removal: 1,003 objects
at 60 imports and 5,083 at 300. Timing from contaminated shared-host runs cannot
support a speed claim.

Build and reproduce an individual case:

```sh
CARGO_BUILD_BUILD_DIR=/tmp/casita-online-holds-build cargo \
  --config 'build.build-dir="/tmp/casita-online-holds-build"' \
  bench --features experimental --bench online_holds --no-run
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 \
  CASITA_BENCH_SCENARIO=imports_readers_gc \
  CASITA_BENCH_READER_SCOPE=application CASITA_BENCH_GC_DELAY_MS=5 \
  taskset -c 8,10,12,14 <saved-online_holds-binary>
# Repeat with CASITA_BENCH_IMPORTS=300.
```

## Validation

`cargo test --all-features --no-fail-fast -j 1` passed: 773 tests across
library, integration, and documentation targets; 28 intentionally ignored.
RustFS was available on `PATH` for the remote-storage tests.
`cargo clippy --all-features --all-targets -j 1 -- -D warnings`, formatting,
and `git diff --check` passed. Logs are retained alongside benchmark artifacts.

## Benchmark results

All 16 runs passed, including four warmups. There were no retries of failed
runs. All integrity and exact-removal gates passed. The table gives medians of
the three measured runs per variant; warmups are excluded.

| Imports | Metric | Before | After |
| --- | --- | ---: | ---: |
| 60 | Import seconds | 3.705 | 3.875 |
| 60 | Reader-open p99, ms | 74.00 | 87.27 |
| 60 | Objects reclaimed during imports | 969 | 986 |
| 300 | Import seconds | 53.475 | 56.140 |
| 300 | Reader-open p99, ms | 85.32 | 76.57 |
| 300 | Objects reclaimed during imports | 5,066 | 5,066 |

Only two runs met the quiet-host criterion; both were 60-import baseline runs.
The other 14 had competing processes or external CPU usage above 5% of host
capacity. Recorded activity includes NetworkManager and desktop processes.
There is no uncontaminated before/after pair. The observed import medians rose
4.6% and 5.0%, respectively; these results cannot establish that the refactor
caused those differences, nor can they rule out a small regression. After-run
import times ranged from 3.828–6.056 seconds at 60 imports and 44.387–56.456
seconds at 300 imports. A performance-neutrality claim needs a quiet rerun.

The correctness result is stronger: eligible cleanup progressed with concurrent
readers and writers, all obsolete objects were reclaimed, and no holds, claims,
collector ownership, or prune fences leaked. The deterministic overlap test
establishes which publication owns each retirement; aggregate removal totals
alone would not prove that property.

The existing corpus covers both workload sizes permanently. This comparison
adds no scheduling option or new benchmark case. Saved comparison commands:

```sh
python3 benchmarks/results/publication-retirements-2026-09-09/run.py
python3 benchmarks/results/publication-retirements-2026-09-09/summarize.py
```

The runner requires the saved, hash-checked baseline and after binaries at the
locations in `protocol.json`. Raw artifacts live under the ignored results
directory; this report and the permanent benchmark definition are versioned.
