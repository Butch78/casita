# Remote pin investigation evidence

Baseline: `1f741ea03328690bd42b8e923f562c5f31c55190`. See the [protocol proposal](../../pin-ledger-protocol.md).
The retained source hashes identify the pre-reorganization measurement tree;
the production probe now lives under `crates/casita/src`.

## RustFS controls

All 15 original HTTP cases and the 2-second timeout control completed. Every acknowledged receipt/content passed fresh uncached HTTP readback. This is one diagnostic run on a shared Linux workstation with other builds, not a throughput ranking or deployed-cluster result. Startup readiness 503s in server logs are excluded from the workload counts.

| Processes | Control | Success responses | 412 | 503 | Median 503 seconds |
|---:|---|---:|---:|---:|---:|
| 1 | disjoint-put (http-default) | 8 | 0 | 0 | n/a |
| 1 | hot-put (http-default) | 8 | 0 | 0 | n/a |
| 1 | hot-cas (http-default) | 16 | 0 | 0 | n/a |
| 10 | disjoint-put (http-default) | 80 | 0 | 0 | n/a |
| 10 | hot-put (http-default) | 79 | 0 | 1 | 5.036 |
| 10 | hot-cas (http-default) | 90 | 70 | 0 | n/a |
| 32 | disjoint-put (http-default) | 256 | 0 | 0 | n/a |
| 32 | hot-put (http-default) | 203 | 0 | 53 | 5.020 |
| 32 | hot-cas (http-default) | 265 | 247 | 0 | n/a |
| 64 | disjoint-put (http-default) | 512 | 0 | 0 | n/a |
| 64 | hot-put (http-default) | 352 | 0 | 160 | 5.015 |
| 64 | hot-cas (http-default) | 525 | 499 | 0 | n/a |
| 100 | disjoint-put (http-default) | 800 | 0 | 0 | n/a |
| 100 | hot-put (http-default) | 556 | 0 | 244 | 5.005 |
| 100 | hot-cas (http-default) | 812 | 788 | 0 | n/a |
| 32 | hot-put (http-lock2) | 237 | 0 | 19 | 2.006 |

CAS success-response counts include GETs; successful conditional PUTs are counted separately as acknowledgments in [http-summary.json](http-summary.json). At 100 clients, only 12 of 800 CAS attempts committed; 788 received 412. All 800 disjoint PUTs succeeded; 244 of 800 unconditional hot-key PUTs returned 503. These are different outcomes.

Compressed raw client events and exact backend executable identity: [default timeout](http-default.json.gz), [2-second timeout](http-lock2.json.gz). Compressed server logs: [default](http-default-server.log.gz), [2-second](http-lock2-server.log.gz). The first run predates error-body retention; the 2-second control retains XML bodies as well as codes. Local RustFS data was isolated under the output artifact directory. The user identified RustFS as the intended production store. These runs use that engine locally; they do not claim to reproduce a separately deployed cluster configuration.

## Actual Casita ledger accounting

[All 40 samples](remote-pin-cost.json) execute the production optimistic edit and checksummed codec with a counted in-memory backend. [Raw log](remote-pin-cost.log). These have 1/10/32/64/100 active pins in one process, not that many independent Casita processes. Register all pins, add resources round-robin, then release all pins. No CAS conflicts or network retries are injected into this probe.

| Active pins | Resources per pin | Batch | GET | PUT | Bytes per direction |
|---:|---:|---:|---:|---:|---:|
| 1 | 64 | 1 | 66 | 66 | 60,642 |
| 1 | 64 | 8 | 10 | 10 | 8,450 |
| 10 | 64 | 1 | 660 | 660 | 5,695,920 |
| 10 | 64 | 8 | 100 | 100 | 789,200 |
| 32 | 64 | 1 | 2112 | 2112 | 58,038,144 |
| 32 | 64 | 8 | 320 | 320 | 8,037,760 |
| 64 | 64 | 1 | 4224 | 4224 | 231,890,688 |
| 64 | 64 | 8 | 640 | 640 | 32,111,360 |
| 100 | 64 | 1 | 6600 | 6600 | 565,909,200 |
| 100 | 64 | 8 | 1000 | 1000 | 78,362,000 |

For 100 pins, batching 64 resources by eight reduces request pairs 84.85% and encoded bytes 86.15%. It does not eliminate quadratic aggregate shared-ledger growth. Compiler identity and binary SHA-256 are recorded with the samples; this run used the host Rust 1.97.1 toolchain rather than the repository's pinned 1.96.0 toolchain. It establishes exact counts, not a compiler performance comparison.

## Independent-process protocol model

The fixed matrix is recorded in [model-fixed.json](model-fixed.json), with a [generated table](model-fixed.md). All 180 cases completed, and all 7,362 acknowledged outputs passed fresh readback. It compares the shared request shape, explicit batching and the proposed writer-owned freeze barrier with concurrent GC at all five process counts. These are executable protocol models using JSON/file CAS, not three implemented Casita backends. Model byte counts and wall times must not be relabelled as S3 results. The matching runner source is retained in [model-source.py.txt](model-source.py.txt).

The first attempted matrix, [model.json](model.json), is rejected: a killed writer could poison the shared multiprocessing result queue. The fixed runner isolates that notification channel. An earlier local smoke test also exposed enumeration of the fixture's atomic-replacement temporary files, fixed before the retained matrices. Rejected samples are not performance evidence.

## Protocol prototypes on RustFS

All 60 cases were attempted at 1/10/32/64/100 independent writer processes: 34 completed and 26 failed. Fresh readback passed for every case. Successful samples checked 531 acknowledged outputs; aborted samples used a new reader to enumerate and validate every durable root, including interrupted acknowledgment messages. A passing failure audit is not a successful liveness or performance sample.

These are JSON protocol prototypes over real HTTP, with one concurrent collector and a 10 ms pause between complete GC passes. The shared variant conservatively freezes all mutation admission, unlike Casita’s fine-grained deletion claims. The writer-owned prototype retains empty records and registry membership. These choices and the shared workstation limit production extrapolation.

| Processes | Shared completed / 4 | Batched completed / 4 | Writer-owned completed / 4 |
|---:|---:|---:|---:|
| 1 | 4 | 4 | 4 |
| 10 | 4 | 4 | 4 |
| 32 | 0 | 3 | 4 |
| 64 | 0 | 0 | 3 |
| 100 | 0 | 0 | 0 |

Normal-operation samples that completed:

| Processes | Design | Writer pin GET | Writer pin PUT | Collector requests | Seconds |
|---:|---|---:|---:|---:|---:|
| 1 | current-shape | 37 | 11 | 68 | 1.54 |
| 1 | batched | 4 | 4 | 36 | 1.39 |
| 1 | owned | 13 | 5 | 49 | 0.78 |
| 10 | current-shape | 980 | 793 | 316 | 37.35 |
| 10 | batched | 293 | 235 | 137 | 8.85 |
| 10 | owned | 944 | 93 | 259 | 6.34 |
| 32 | batched | 2577 | 2228 | 579 | 58.02 |
| 32 | owned | 2167 | 556 | 764 | 10.47 |
| 64 | owned | 2962 | 1891 | 1193 | 35.18 |

At 10 writers, explicit batching reduced observed writer pin requests from 1,773 to 528, a 70.2% reduction in this workload. Writer-owned records used 1,037, including polling while frozen. Collector requests are shown separately; final fixture recovery/audit requests are outside those workload counters. Request savings are not uniformly determined by the number of successful PUTs.

Failures are retained, not averaged into throughput or relabelled as successful runs. At 32 writers the shared variant exhausted its per-edit budget in all four faults, while batching completed three. At 64 writers the writer-owned crash case failed registry admission under repeated collection. This exposes admission fairness and GC duty cycle as additional constraints beyond protection-key contention. Failed-sample metrics describe only the triggering writer, not complete aggregate traffic.

Of the 26 failed cases, 15 were triggered by ledger edit-budget exhaustion, five by registry admission-budget exhaustion, and six by backend HTTP 503s on ledger GETs at 100 writers. Those six responses took 5.002 to 5.011 seconds. The earlier 32/64-writer triggering writers reported zero 503s. A GET failure is not an ambiguous commit; this fixture deliberately fails the sample on non-conflict backend errors instead of adding transport retries.

See [summary with failure evidence and latency quantiles](rustfs-protocol-summary.json), [initial raw run](rustfs-protocol-initial.json.gz), [remaining matrix](rustfs-protocol-remaining.json.gz), and [remaining server log](rustfs-protocol-remaining-server.log.gz). The initial `benchmark all` run completed the 1/10-writer cases and stopped at a 32-writer failure. The revised supervisor reran all 32/64/100 cases, continued after errors and audited partial publications. Its `attempted_all=true` and `complete=false` intentionally distinguish coverage from success. Original [all-suite completion ledger](rustfs-protocol-all-execution.json.gz) and [failure log](rustfs-protocol-initial.log.gz) are retained.

The initial fixture was subsequently restarted and all 25 case prefixes audited using the same permanent `audit_all_roots` correctness gate. All 120 durable roots passed fresh readback, including examination of the aborted 32-writer prefix. The [recovery audit](rustfs-initial-recovery-audit.json) records this separately from performance results. Six startup LIST 503s were retried as reads before auditing; these are outside the benchmark workload. The [audit invocation](rustfs-initial-recovery-audit.py.txt) and [restarted server log](rustfs-initial-recovery-server.log.gz) are retained.

## Validation

* [Lease tests](lease-tests.log): ten passing production tests, including cancelled writers, cancelled batch waiters and conflicting deletion claims.
* [Focused Python tests](harness-tests.log): protocol safety schedules, manifest discovery and benchmark-all integration.
* [Full Python harness suite](all-harness-tests.log): 311 tests, with two skips, including the RustFS prototype extension. A later [barrier rerun](barrier-tests.log) also checks stale CAS after thaw/reopen restores the original payload.
* `cargo fmt --all -- --check` and `git diff --check` were run separately.

## Reproduction and scope

Run the commands in the [proposal](../../pin-ledger-protocol.md#permanent-experiments-and-remaining-gates) from the development shell. The HTTP artifacts use RustFS 1.0.0-rc.1. The model uses independent spawned processes, fsynced conditional local objects, one concurrent collector and a fresh spawned reader after collection. Raw output includes process-count, fault, GC, request, byte and acknowledgment evidence.

Still required: the real Casita three-implementation end-to-end process matrix, collector-crash recovery testing, and arbitrary delayed/ambiguous network outcomes. The proposal does not represent these unimplemented gates as completed. The later [implementation follow-up](../2026-09-20-pin-batching-retries/README.md) added a typed pre-append retry policy, measured batching, and checkpoint failure-injection tests. Durable reconciliation of indeterminate commits remains separate work before promoting writer-owned records.

The raw HTTP matrix and two-second control were repeated successfully through `benchmark all`: [completion ledger](all-execution.json.gz), [HTTP events](all-pin-http.json.gz), [two-second control](all-pin-http-lock2.json.gz). At 100 clients the repeat again had zero disjoint-key 503s, 200 hot-key PUT 503s and 789 conditional conflicts. The exact failure counts vary with scheduling and load.
