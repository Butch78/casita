# Process-owned local reader protection

Ordinary local `Repository::open` now uses process-owned read protection. Warm
admission, pack protection, and release replace a bounded reader inventory
atomically without syncing the durable ledger. Readers continue to retain only
the selected object's closure and required physical packs.

The previous object-scoping work improved GC progress but added durable pin
handoff costs. These measurements compare the new path with a test-only
`durable-object` control in the same optimized binary. Both use the same narrow
GC contract and physical read plan. This is a local implementation comparison,
not a comparison between two committed revisions.

## Repeated local measurements

Three repetitions per configuration on an AMD Ryzen 7 7840S, Linux 7.0.10,
encrypted Btrfs, performance CPU governor. No agent-started builds or tests
overlap this repeated comparison. The workstation is not a dedicated benchmark
host. OS caches are not flushed; the Casita pack cache is disabled.

Medians in milliseconds; speedup compares total open latency. Open includes
admission and physical resolution; temporary snapshot cleanup is measured as a
separate handoff phase in the raw results. Release includes draining queued
leases. Setup, correctness audits, and subsequent GC are outside these timings.

| Payload bytes | Garbage objects | Durable open ms | Process open ms | Open speedup | Durable release ms | Process release ms |
|---:|---:|---:|---:|---:|---:|---:|
| 64 | 1 | 28.165 | 0.882 | 31.9× | 14.046 | 0.946 |
| 64 | 64 | 30.004 | 1.572 | 19.1× | 13.742 | 0.867 |
| 1,048,593 | 1 | 43.615 | 0.912 | 47.8× | 14.028 | 0.596 |
| 1,048,593 | 64 | 45.545 | 1.685 | 27.0× | 14.038 | 0.644 |

Every warm object open and release left the durable ledger unchanged. Every
durable control open and release changed it. Both object modes reclaimed all
unrelated logical objects while the reader remained open. The 64-garbage cases
reclaimed 2,102,912 physical pack bytes before reading and seeking
through the retained object after collection and vacuum. Dropping the reader
allowed final collection, with no leaked pins.

Three repetitions establish a local signal, not a universal latency guarantee.
Raw per-phase samples, process logs, environment metadata, and the artifact hash
are retained in [the comparison JSON](2026-09-09-process-reader-comparison.json).

## Coordination and reservation boundary

Medians of three processes per configuration; warm values within each process
average eight register/protect/release cycles. The count is the number of other
active readers for warm and boundary operations. Cold always starts with no
process reader owner. Boundary setup positions the production counter at its
last reserved revision outside timing, then measures the real renewal path.

| Other active readers | Operation | Median ms |
|---:|---|---:|
| 1 | cold-register | 18.952 |
| 1 | warm-register | 0.141 |
| 1 | warm-protect | 0.142 |
| 1 | warm-release | 0.135 |
| 1 | last-reserved-register | 0.322 |
| 1 | reservation-rollover-protect | 14.954 |
| 1 | renewed-release | 0.259 |
| 64 | cold-register | 19.155 |
| 64 | warm-register | 0.154 |
| 64 | warm-protect | 0.149 |
| 64 | warm-release | 0.151 |
| 64 | last-reserved-register | 0.389 |
| 64 | reservation-rollover-protect | 14.837 |
| 64 | renewed-release | 0.243 |

Cold registration and reservation rollover change the durable ledger. Warm
operations, the last reserved registration, and the release after renewal do
not. The reservation covers 65,536 subsequent combined ledger transitions;
durable writes also consume revision numbers. First-owner setup and occasional
renewal therefore remain visible durability costs.

[Coordination JSON](2026-09-09-reader-coordination.json) retains raw iterations
and gates: preserved durable staging, exact reservation renewal, rejected stale
and protected deletion claims, and no leaked reader pins.

## Protocol and correctness

- Independent handles share a kernel-locked owner in one process. PID reuse and
  timeouts do not decide liveness. Inherited handles are rejected after fork.
- Reader updates and durable prune/deletion claims use the same file lock and
  combined revision. Dropping read-only protection does not retain obsolete
  broad snapshot pins for the rest of an active collection.
- Staging, explicit retained sessions, collector ownership, and deletion claims
  remain durable. Remote readers retain their existing durable fallback.
- A missing or corrupt reader inventory fails closed while owners are live.
  After all owners exit, recovery fences the entire reserved clock, including
  when a host crash leaves an intact but older reader file. It preserves durable
  recovery records and requires no allocation of a replacement reader inventory.
- `CASPIN04` fences older collectors. After activation, older binaries reject the
  ledger; all processes sharing a repository must be upgraded together.

Validation: 617 library tests passed, including seven killed-process publication
boundaries, cancellation during publication, cross-process GC, stale-clock
recovery, blocked reader-inventory replacement, and the reservation boundary.
The local/S3 application API tests (8), benchmark runner tests (168), formatting,
and all-feature/all-target Clippy also passed. There are 24 intentionally ignored
benchmark tests in the library suite.

The final [benchmark-all completion ledger](2026-09-09-reader-all.json) records
both registered suites passing. Its 72 object cases cover empty, tiny,
65,535/65,536/65,537-byte, and multi-chunk payloads, cold/warm admission, all three
retention modes, and 1/4 garbage objects. Smoke timings are correctness coverage;
some overlap validation work and are not used for the repeated comparison above.
[Smoke object results](2026-09-09-reader-smoke.json) and
[smoke coordination results](2026-09-09-reader-coordination-smoke.json) are retained.

The reader inventory is still rewritten per transition; its cost scales with
active readers and owners. New reader admission still needs space for its atomic
inventory replacement. Durable GC and dead-reader recovery keep their existing
preallocated-ledger path. Incremental durable journaling and publication group
commit remain subsequent work. Metadata backup/import design remains deferred.

## Reproduce

```console
cargo test --release --all-features --lib --no-run --message-format=json
benchmark all --suites object-reads,reader-coordination --profile smoke --repetitions 1 --bin-dir /tmp/casita4-process-reader-bin --output benchmarks/results/readers-all
benchmark run object-reads --sizes 64,1048593 --garbage-counts 1,64 --modes object,durable-object --admissions warm --repetitions 3 --probe-binary /tmp/casita4-process-reader-bin/casita-lib-test --no-build --output benchmarks/results/reader-comparison.json
benchmark run reader-coordination --profile smoke --repetitions 3 --probe-binary /tmp/casita4-process-reader-bin/casita-lib-test --no-build --output benchmarks/results/reader-coordination.json
cargo test --all-features --lib -- --test-threads=4
cargo test --all-features --test application_api --test s3_application_api -- --test-threads=4
python3 -m unittest discover -s benchmarks/tests
cargo clippy --all-features --all-targets -- -D warnings
```

Copy the library test executable emitted by Cargo to the indicated immutable
binary directory, or omit the binary flags and let the suite build it. All final
performance samples use SHA-256 `87ee97a7bf2d13a658e84a6df7c34072ce48ee21b8bd865eea49df23096ad891`. The source is the local worktree based
on `ade46e004c58354e4ffbd959642a0eccd48ce4c7`; raw reports mark it dirty.
