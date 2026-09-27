# Online holds contention experiment, 2026-09-08

The initial failure below is addressed by the [contention fix and rerun](#contention-fix-and-rerun).

The new `online_holds` benchmark passes its small smoke workload, but the
standard workload exposed an import failure when a reader and GC run together.
This is preliminary diagnostic evidence, not a performance baseline.

Run with `cargo bench --features experimental --bench online_holds` or
`benchmark run online-holds`. See the [benchmark guide](../README.md#online-holds-under-contention)
for the workload and metric definitions. Inputs use the generic filesystem
importer through a mutation session, allowing admission to be timed separately.

The retained standard run used 30 imports of 16 unique 4 KiB files per scenario,
on Linux/btrfs with an AMD Ryzen 7 7840S (16 logical CPUs), four Tokio runtime
workers, and rustc 1.97.1. Source was `ac2eba5` plus the uncommitted admission
helper refactor and this benchmark. Other CPU-heavy jobs were active. An earlier
import-only run achieved 13.97 files/s versus 7.25 in the retained run; absolute
timing and comparisons are therefore not stable enough to choose an optimization.

| Scenario | Files/s | Writer admission p95 (ms) | Ledger revisions/file | Successful GC passes | Busy / other retryable GC errors |
| --- | ---: | ---: | ---: | ---: | ---: |
| Imports | 7.25 | 284.09 | 2.44 | 0 | 0 / 0 |
| Imports + reader | 4.86 | 524.50 | 3.97 | 0 | 0 / 0 |
| Imports + GC | 5.93 | 550.58 | 2.89 | 11 | 317 / 49 |
| Imports + reader + GC | Failed | — | — | — | — |

Both import-only runs produced exactly 1,170 ledger revisions: 39 per 16-file
import. These are durable inventory revision changes, not physical disk writes.
GC-only reported 17 removals in successful passes and 204 in final cleanup,
compared with 493 cleanup removals without concurrent GC. Retryable collection
failures can happen after partial progress, so successful-pass counters alone
undercount reclamation.

The combined scenario failed inside the generic filesystem import with:

```text
Payload(Backend(Payload(Io(Custom { kind: Other, error: Transient("pin ledger remained contended") }))))
```

The benchmark deliberately does not retry failed imports. GC retries only errors
classified as retryable by the existing API and counts them separately from busy
admission. This failure warrants investigating ledger contention/backoff and
nested error classification before using these numbers to justify batching. It
has been observed once in the standard run; it is not yet a deterministic test.

The three completed standard scenarios passed fsck, pin cleanup checks, and
checkout byte verification. All four scenarios also passed with three imports
of two files, both directly and through the suite runner. This smoke result does
not establish reliability at the standard load.

Local raw evidence (ignored generated output):

- `benchmarks/results/online-holds-2026-09-08/`: full-run stdout, failed execution
  ledger, executable hash, and environment metadata. The initial runner saved
  completed rows only in stdout on failure; the updated runner also retains
  completed rows in JSON, covered by a regression test.
- `benchmarks/results/online-holds-smoke-2026-09-08/`: successful suite-runner smoke
  measurements and validation ledger.

## Contention fix and rerun

Local ledger edits previously released a shared read lock before reacquiring an
exclusive lock for compare-and-swap. Active readers and collectors could change
the revision in that gap on every attempt, exhausting the 32-attempt limit.
Local updates now hold one exclusive file lock across the read, in-memory
transition, and durable write. The lock covers only this short transaction;
read holds and GC passes do not retain it. Remote ledgers retain conditional
writes. Typed retry guidance also survives nested payload, backend, and I/O
wrappers, including retry-after delays; corruption remains non-retryable.

The benchmark additionally checked for leaked pins too early: an interrupted
collection deliberately retains claims and retired pins until recovery. It now
performs final collection/recovery before the leak check and fsck. This does not
change the timed workload or suppress failed imports.

All four standard scenarios passed after these changes, using the same 30 by 16
workload. Results are retained in
`benchmarks/results/online-holds-fixed-2026-09-08/`, including raw output,
executable hash, source changes, and environment metadata (captured after run).

| Scenario | Files/s | Writer admission p95 (ms) | Ledger revisions/file | Successful GC passes | Busy / other retryable GC errors |
| --- | ---: | ---: | ---: | ---: | ---: |
| Imports | 8.72 | 150.86 | 2.44 | 0 | 0 / 0 |
| Imports + reader | 7.07 | 284.78 | 3.40 | 0 | 0 / 0 |
| Imports + GC | 10.62 | 211.12 | 2.68 | 8 | 47 / 24 |
| Imports + reader + GC | 12.29 | 180.76 | 3.46 | 12 | 103 / 0 |

Every scenario passed recovery, pin cleanup, fsck, and final checkout byte
verification. The combined scenario reclaimed no obsolete objects before final
cleanup in this run; successful passes do not guarantee reclamation during
continuous mutation. Its higher throughput than the import-only case also
illustrates the changing machine load: these figures are diagnostic, not a
controlled speedup comparison. This is one successful standard rerun, not a
statistical reliability claim.

Regression coverage includes simultaneous edits through 40 independent local
ledger handles, the exact nested error pattern from the failure, and filesystem
imports alongside independent reader and collector repository handles. Imports
and reads are not retried by that integration test.

Validation covered all 573 non-ignored library tests (17 ignored), 11 distinct
online collection/cancellation/process/S3 integration tests, and the core error
test with native features disabled. The initial broad library command omitted
RustFS from PATH: 539 passed and 34 failed to start that fixture. Rerunning the
entire 59-test WAL group with RustFS available resolved all 34 failures.
All-features/all-targets Clippy with warnings denied, formatting, and diff checks
also passed.
