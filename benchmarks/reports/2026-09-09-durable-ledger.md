# Incremental local durable ledger

Linux `FilePinStore` now appends checksummed deltas and shares a durability barrier across up to 64 queued operations. Checkpoints reuse two preallocated files. Ordinary object-scoped reader protection and the public metadata APIs retain their contracts. Backup/export work remains deferred.

## Optimized comparison

Three repetitions per configuration, four iterations per process, randomized execution order. Both modes use the same optimized binary: the journal and a test-only control using the previous inventory-replacement path. The control also uses the shared operation application helper; this isolates the physical persistence and batching change rather than comparing two committed revisions.

AMD Ryzen 7 7840S, Linux 7.0.10, encrypted Btrfs, performance governor. These are local workstation results, not a dedicated benchmark host or a latency guarantee. No agent-started build or test overlapped the measurements. Filesystem caches were not flushed. Variability across runs is retained in the raw samples.

Each value is the median across processes of phase completion time divided by logical operations. Under contention this is **amortized time per operation, not individual request latency**. Setup, deletion rejection gates, and exact cold-replay audits are outside timing. Process RSS includes them.

### Registration

| Retained records | Writers | Replacement ms/op | Journal ms/op | Speedup |
|---:|---:|---:|---:|---:|
| 1 | 1 | 15.011 | 7.542 | 2.0× |
| 1 | 8 | 15.565 | 0.854 | 18.2× |
| 1 | 64 | 18.407 | 0.146 | 126.1× |
| 64 | 1 | 27.405 | 7.401 | 3.7× |
| 64 | 8 | 15.428 | 0.681 | 22.7× |
| 64 | 64 | 16.236 | 0.196 | 82.8× |
| 4,096 | 1 | 21.230 | 11.782 | 1.8× |
| 4,096 | 8 | 32.509 | 3.987 | 8.2× |
| 4,096 | 64 | 20.769 | 2.707 | 7.7× |

### Separate phases: 64 retained records, eight writers

Payload/publication protection means ledger pin extensions. This probe does not measure actual payload sealing or metadata publication, and these results do not establish an end-to-end import speedup. Deletion phases each contain one operation.

| Phase | Replacement ms/op | Journal ms/op | Speedup |
|---|---:|---:|---:|
| register | 15.428 | 0.681 | 22.7× |
| payload-protect | 15.520 | 0.662 | 23.5× |
| publication-protect | 15.557 | 0.548 | 28.4× |
| release | 16.413 | 0.530 | 30.9× |
| deletion-claim | 15.634 | 4.036 | 3.9× |
| deletion-finish | 16.182 | 3.678 | 4.4× |

The large-inventory cases expose remaining CPU work: at 64 writers, registration rises from 0.196 ms/op with 64 retained records to 2.707 ms/op with 4,096. Operations still clone/walk inventory in memory. Ordinary updates avoid whole-inventory encoding and replacement, but are not constant-time in retained state. This is the next scaling cost to profile.

[Comparison JSON](2026-09-09-durable-ledger-comparison.json) contains all six phases, raw iterations, counters, process logs, environment, and binary hash.

## Checkpoint, batching, and capacity boundaries

Every row is a median of three processes. The operation-limit warmup is grouped and remains below half the byte window, so it independently verifies the 256-operation bound. Byte cases append 256 KiB catalog records: the fourth would cross the 1 MiB frame window. Batch cases invoke the production executor on a blocking worker; asynchronous queue arbitration is excluded.

| Boundary | Position | ms | Frames | Append/checkpoint syncs | Checkpoints |
|---|---:|---:|---:|---:|---:|
| checkpoint-operations | 255 | 3.963 | 1 | 1 | 0 |
| checkpoint-operations | 256 | 3.789 | 1 | 1 | 0 |
| checkpoint-operations | 257 | 13.880 | 0 | 2 | 1 |
| checkpoint-bytes | 3 | 5.491 | 1 | 1 | 0 |
| checkpoint-bytes | 4 | 16.616 | 0 | 2 | 1 |
| checkpoint-bytes | 5 | 5.682 | 1 | 1 | 0 |
| group-size | 63 | 5.495 | 1 | 1 | 0 |
| group-size | 64 | 5.459 | 1 | 1 | 0 |
| group-size | 65 | 9.098 | 2 | 2 | 0 |
| migration-space | 0 | 51.953 | 0 | 0 | 0 |
| migration-space | 1 | 34.770 | 0 | 2 | 1 |

Migration position 0 denies journal capacity growth and verifies one durable legacy replacement; position 1 permits journal activation. The denial is injected, not a physically full device. Zero journal syncs in the legacy fallback does **not** mean zero durability barriers: the replacement path performs its own syncs. Journal sync counters exclude capacity-growth, adoption, and migration preparation barriers; raw counters distinguish replacements and adoptions. `max_group` is a cumulative store high-water mark.

Checkpoint and 65-operation batch costs are visible rather than averaged away. All cases verify exact replay and no leaked protection. [Boundary JSON](2026-09-09-ledger-boundaries.json) retains all samples.

## Reader and recovery gates

- Eight object-reader cases cover 64-byte and 1,048,593-byte payloads, 1/64 unrelated garbage objects, and cold/warm admission. Warm open and release preserve durable ledger bytes. Readers survive independent GC/vacuum with exact bytes and seeking, while unrelated logical and physical garbage is reclaimed. [Reader results](2026-09-09-journal-object-reads.json).
- Coordination cases cover 1/64 active readers and both sides of the 65,536-transition reservation. Warm operations remain unsynced; renewal remains durable. [Coordination results](2026-09-09-journal-reader-coordination.json).
- Publisher kills at eight append/checkpoint/reply phases retain acknowledged staging pins and deletion claims. Four independent processes append concurrently without losing updates. Cancellation waits for durability before token cleanup; complete-frame corruption and corrupted epochs fail closed.
- Allocation-denied tests reuse checkpoint inodes and permit bounded GC transitions, reject growth without losing pins, and preserve legacy collection before migration.
- `cargo test --all-features` passed: 628 library tests, plus CLI, integration, and doctest suites. Real full-filesystem `ENOSPC` tests and process-abort GC recovery ran successfully on this host. The final benchmark-only warmup adjustment was rechecked with `cargo test --all-features --lib checkpoint_and_group_boundaries`.
- `cargo clippy --all-features --all-targets -- -D warnings`, `cargo check --no-default-features`, formatting, and all 171 Python corpus tests passed.
- Both new suites passed through `benchmark all`; the [completion ledger](2026-09-09-ledger-all-execution.json), [durable smoke data](2026-09-09-ledger-all-durable.json), and [boundary smoke data](2026-09-09-ledger-all-boundaries.json) preserve that run. Artifact hashes remained unchanged.

## Reproduce

Build from this worktree change atop `54da616d4dca2aae23cc41cf7f5318d9b7a5168f`. The recorded optimized binary SHA-256 is:

```text
7bff317bb1ac73422f491238051f0ab0341e54ea2c6193a055b3a38abfcf6582
```

```sh
cargo test --release --all-features --lib --no-run --message-format=json
# Copy the emitted library test executable to /tmp/casita4-journal-bin/casita-lib-test.
python3 -m benchmarks.cli run durable-ledger --counts 1,64,4096 --writers 1,8,64 --iterations 4 --repetitions 3 --probe-binary /tmp/casita4-journal-bin/casita-lib-test --no-build --output /tmp/durable-ledger.json
python3 -m benchmarks.cli run ledger-boundaries --repetitions 3 --probe-binary /tmp/casita4-journal-bin/casita-lib-test --no-build --output /tmp/ledger-boundaries.json
python3 -m benchmarks.cli run object-reads --sizes 64,1048593 --garbage-counts 1,64 --modes object --admissions cold,warm --repetitions 1 --probe-binary /tmp/casita4-journal-bin/casita-lib-test --no-build --output /tmp/journal-object-reads.json
python3 -m benchmarks.cli run reader-coordination --profile smoke --repetitions 1 --probe-binary /tmp/casita4-journal-bin/casita-lib-test --no-build --output /tmp/journal-reader-coordination.json
python3 -m benchmarks.cli all --suites durable-ledger,ledger-boundaries --profile smoke --repetitions 1 --bin-dir /tmp/casita4-journal-bin --output /tmp/journal-all
```

The production durability/format protocol and its limitations are described in [the local journal protocol](../../docs/local-pin-journal.md). Older binaries reject the new journal format; upgrade all processes before activation. Non-Linux and object-store persistence retain their existing paths.
