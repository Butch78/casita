# Repeated online-GC validation

The complete all-feature test suite passes, but longer repeated benchmarks
do **not** establish useful GC progress during the combined import/read
workload. No pass completed during imports reported any reclamation in
any of the three repetitions.

## Test results

- `cargo test --all-features`: 688 passed, zero failed, 17 ignored.
- Includes library, CLI, integration, WAL/S3, Turso multiprocess, process-crash,
  and documentation tests. RustFS was available on PATH.
- All-feature/all-target Clippy with warnings denied passed.
- Formatting and diff checks passed.
- All 12 benchmark scenarios completed, including sentinel reads, final
  collection, leak checks, fsck, and byte-verified checkout.

The only code change in this validation step is a benchmark output field:
`gc_completed_passes` records each successful pass's completion time and
reported removals. Production GC behavior was not changed.

## Workload and reproduction

Three sequential repetitions of all four benchmark scenarios. Each uses
60 imports × 16 unique 4 KiB files, twice the previous 30-import workload.
Each repetition creates fresh repositories with the same deterministic
input corpus. There are 1,003 obsolete logical objects per scenario.

```sh
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' bench --features experimental --bench online_holds --no-run
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 CASITA_BENCH_READER_SCOPE=object   /tmp/casita-online-holds-build/release/deps/online_holds-6a75bc045fd4d5b4
# Run the executable three times, retaining each output.
```

Tests finished before benchmark execution. Other users' workloads remained
active on this shared machine. The existing five-minute per-scenario timeout
was retained; no scenario timed out or was replaced by a shorter run.

## Combined imports, readers, and GC

| Repetition | Import seconds | Successful passes during imports | Removed in those passes | Removed in passes completed afterward | Final cleanup removed | Logical-pin Busy | Admission Busy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 84.33 | 0 | 0 | 1003 | 0 | 25 | 1 |
| 2 | 78.07 | 0 | 0 | 1003 | 0 | 26 | 0 |
| 3 | 268.79 | 3 | 0 | 119 | 884 | 4 | 0 |

Repetitions 1 and 2 each completed only one successful pass, after imports
stopped. Repetition 3 completed three zero-removal passes near the start,
then no further successful pass until after imports stopped:

| Repetition | Successful pass completion seconds → reported removals |
| --- | --- |
| 1 | 85.240 → 1003 |
| 2 | 78.899 → 1003 |
| 3 | 1.470 → 0; 2.854 → 0; 3.475 → 0; 270.841 → 119 |

Repetition 3 also reported one retryable stale metadata revision. Repetitions
1 and 2 reported no additional retryable errors. All combined runs account
for 1,003 obsolete objects through their successful reports plus final cleanup.

These timestamps measure pass completion, not individual deletion times.
An after-import pass may have performed some deletions earlier. Consequently,
the result is specifically **zero successful reclamation passes completed
during imports**, rather than proof that no individual deletion happened then.

## Imports and GC without readers

| Repetition | Import seconds | Successful passes during imports | Reported removals during imports | Retryable errors | Final cleanup removed |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 51.13 | 10 | 68 | 51 | 578 |
| 2 | 80.77 | 8 | 17 | 66 | 680 |
| 3 | 97.63 | 9 | 34 | 55 | 357 |

Without readers, successful passes reported reclamation during imports in
all three runs. No Busy attempts occurred. Retryable passes can make partial
progress before failing, so the successful-pass totals undercount deletion
and do not sum with cleanup to 1,003.

## Conclusion and limits

The earlier 30-import run's 289 reported removals during imports was a valid
observation, but it was not consistently reproduced at this longer workload.
The correctness checks passed; useful progress under sustained concurrent
readers and imports remains unresolved. Logical protection changes remain
the dominant reported Busy reason, with one exhausted admission retry budget.

Before claiming this online-GC work is complete, investigate the remaining
new logical roots that are absent from the retained mark and whether they
represent existing unmarked objects or unpublished objects absent from the
marked metadata snapshot. Any relaxation must continue protecting existing
unmarked objects and enforcing the metadata revision and deletion fences.

This is three runs on one shared workstation, not a proof of starvation under
every schedule or a controlled speed comparison. Load varied substantially;
the third combined import interval lasted 268.79 seconds. There is no basis
for attributing that timing difference to a particular GC path from these
measurements alone.

## Evidence

`benchmarks/results/online-holds-repeated-2026-09-08/` contains:
per-repetition raw logs and parsed JSON, aggregate measurements, exact command
and executable hash, before/after load snapshots, tracked diff, full test log,
and Clippy log. Successful-pass counts, removal totals, active-pass splits,
and reason counts were checked against each emitted scenario.
