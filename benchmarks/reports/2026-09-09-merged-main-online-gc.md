# Concurrent GC on merged main

Production code is `c84c583`, including journaled durable pins, grouped updates,
process-owned application readers, and the online-GC admission/cleanup fixes.
Only the permanent benchmark and its registration/documentation changed for
this investigation. Earlier [ledger latency](2026-09-09-ledger-latency.md)
measurements predate this combination and describe replacement-file storage.

## Workload and reader paths

Six sequential runs use 60 generic filesystem imports × 16 unique 4 KiB files,
one reader, concurrent GC, four runtime workers, and the existing five-minute
scenario timeout. Imports replace one root, creating 1,003 obsolete logical
objects. Setup and final integrity/cleanup are outside the import timer.

Three runs retain the historical `object` mode, using the experimental
`open_payload` path. Three use the new `application` mode, opening through the
ordinary application API and its process-owned protection. The application
facade opens a second handle to the same repository before timing. Run order is
object-1, application-1, application-2, object-2, object-3, application-3.

The permanent `online-holds` benchmark now defaults to `application`, including
through `benchmark all`. Historical `object` and full `snapshot` modes remain
selectable through `CASITA_BENCH_READER_SCOPE`. The manifest entry and README
commands describe the default. Reproduce either measured path with:

```sh
CASITA_BENCH_IMPORTS=60 CASITA_BENCH_FILES=16 \
CASITA_BENCH_READER_SCOPE=application CASITA_BENCH_SCENARIO=imports_readers_gc \
cargo --config 'build.build-dir="/tmp/casita-online-holds-build"' \
  bench --features experimental --bench online_holds
```

Replace `application` with `object` for the historical path. The retained runner
executes the built artifact directly, after compilation and checks finish.

## Results

Every run passed sentinel reads, fsck, byte-verified checkout, final pin/claim
cleanup, and attempt/phase accounting. Reported removals plus final cleanup
account for all 1,003 obsolete logical objects in every run.

| Reader path | Run | Import seconds | Reader p99 ms | Removed during imports | Removed afterward / final cleanup | Useful active passes | Snapshot-generation conflicts | Stale-revision retries |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| application | 1 | 2.684 | 31.643 | 782 | 0 / 221 | 5 | 40 | 3 |
| application | 2 | 2.908 | 32.879 | 850 | 0 / 153 | 7 | 30 | 3 |
| application | 3 | 6.622 | 29.159 | 833 | 0 / 170 | 5 | 7 | 0 |
| object | 1 | 3.313 | 41.683 | 969 | 17 / 17 | 44 | 0 | 13 |
| object | 2 | 2.595 | 31.145 | 969 | 0 / 34 | 43 | 0 | 15 |
| object | 3 | 4.914 | 57.349 | 969 | 17 / 17 | 42 | 0 | 15 |

Application readers observe 29–33 ms p99 and reclaim 78–85% of obsolete logical
objects during imports. The historical path reclaims 97%, with 31–57 ms p99.
Both repeatedly reclaim data while imports are active. Application useful-pass
completions span 0.18–2.18, 0.22–2.57, and 1.58–5.66 seconds respectively.

No catalog-cleanup errors occurred. All Busy results in application mode are
`snapshot generation advanced during collection mark`. Object mode has no Busy
results. All other retryable failures are stale repository revisions.

These are logical-object removal counts, not timestamps or byte counts for
physical disk-space reclamation. A successful pass can defer physical cleanup.
The shared host and six short runs do not establish a controlled speedup or
prove one reader mode universally faster. Observed latency is substantially
below the earlier replacement-ledger runs (460–521 ms p99), but journal and
reader changes were not isolated against that older implementation here.

## Journal and group timings

All ledger operations during the joined workload and release drain contribute
to these metrics. Durations overlap across concurrent requests; group wait
includes queueing, execution, and reply. Group totals include kernel lock wait.
These are not additive pieces of import wall time. Journal append sync counts
exclude checkpoint, capacity-growth, and adoption syncs.

| Path / run | Append syncs | Append-sync seconds | Checkpoints | Checkpoint seconds | Group-wait p99 ms | Exclusive-lock p99 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| application / 1 | 925 | 1.008 | 22 | 0.052 | 7.640 | 2.641 |
| application / 2 | 900 | 1.175 | 24 | 0.061 | 7.617 | 3.028 |
| application / 3 | 892 | 3.765 | 23 | 0.194 | 16.862 | 6.352 |
| object / 1 | 1297 | 1.792 | 38 | 0.099 | 10.413 | 3.501 |
| object / 2 | 1389 | 1.396 | 35 | 0.077 | 4.820 | 1.508 |
| object / 3 | 1459 | 3.428 | 40 | 0.255 | 21.265 | 6.666 |

Application-mode collector admission totals 0.38, 0.43, and 1.17 seconds per
run. Its release barriers total 0.05, 0.07, and 0.15 seconds; backend collector
leases are negligible. The number of useful GC passes differs between modes,
so fewer append syncs cannot be attributed solely to the reader implementation.

## Next investigation

`Repository::open_object_inner` currently acquires a temporary snapshot-wide
pin through `pin_metadata_snapshot_kind(..., None)`, then reads the selected
object, acquires its closure/physical protection, and drops the broad pin.
Logical prune rejects newly observed snapshot generations. This temporary hold
is the source-level candidate for the application-only conflicts measured here;
the benchmark records the rejection reason but not each offending pin identity.

Investigate narrowing that temporary logical pin to the requested object while
retaining catalog and physical protection through handoff. Do not relax general
snapshot-generation checks: explicit retained sessions require their broader
protection. Validate absent objects, previously unmarked objects, linked
closures, catalog changes, reader cancellation, and cross-process GC races
before comparing the same workload again. No production optimization is made
in this benchmark change.

## Validation and retained evidence

All-feature/all-target Clippy with warnings denied and formatting/diff checks
pass. All five benchmark-runner tests pass. An actual permanent-corpus smoke
run (`benchmark all --suites online-holds --profile smoke`) passes all four
scenarios and verifies that every row records `application` mode. The merged
production revision had already passed 765 tests before these benchmark-only
changes; that full suite was not repeated here.

`benchmarks/results/merged-main-online-gc-2026-09-09/` retains the executable,
its SHA-256, base revision and benchmark patch, build/check logs, run order,
platform/load snapshots, six raw logs and JSON results, summary script/output,
and the permanent-corpus smoke completion ledger. The executable hash was
checked again after measurement.
