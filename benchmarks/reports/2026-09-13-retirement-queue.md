# Bounded temporary storage for retirement queues

Cleanup now consumes its captured hash set once, keeping up to 1,000 pending paths across awaits. Successful batches discard completed paths and return protected paths to the shared queue. Cancellation or error restores the pending batch and the unvisited iterator tail, preserving retirements published meanwhile. Paths already referenced by the current catalog are discarded after a successful batch. A failed batch may conservatively requeue those paths for rechecking.

The old implementation cloned every queued path into a temporary vector before processing batches and removed completed paths from its owned hash set. The new implementation avoids that full vector and per-path hash removals. It does not repeatedly rescan the hash set and never revisits newly queued or protected paths in the same pass.

This bounds temporary path copies by batch size, not total GC memory. The captured hash-table allocation lives until the iterator is dropped; the retirement queue and pin/catalog inventories still scale with their contents. Cancellation may allocate while restoring unvisited paths to the shared queue, as before. Public APIs, pin scopes and deletion claims are unchanged.

At 100,000 paths, measured median RSS growth fell from 23.16 to 8.67 MiB (14.49 MiB lower), and queue-only runtime fell from 672.52 to 556.60 ms (17% lower). Small-case fixed RSS overhead also differs between binaries/cohorts; roughly 12 MiB of size-dependent growth disappears when comparing 999 versus 100,000 paths. Local filesystem timing changed substantially between cohorts, so its apparent speedup should not be attributed solely to queue iteration.

## Queue-only memory and time

Three samples per cell, median (minimum–maximum). RSS growth is RSS at the paused first deletion minus RSS before cleanup, in MiB. It includes fixed async runtime/code-page overhead and is not an allocation counter. There are no payload files or durable pin-ledger operations in this probe; all candidates are visited exactly once and the queue must end empty.

| Paths | Before RSS growth MiB | After RSS growth MiB | Before ms | After ms |
| ---: | ---: | ---: | ---: | ---: |
| 999 | 10.97 (10.80–10.97) | 8.73 (8.69–9.25) | 8.90 (7.70–10.18) | 5.78 (5.58–5.90) |
| 1001 | 10.37 (9.82–10.58) | 8.93 (8.92–9.17) | 8.58 (8.18–9.28) | 5.56 (5.44–6.32) |
| 100000 | 23.16 (22.50–23.47) | 8.67 (8.64–9.21) | 672.52 (658.87–676.00) | 556.60 (548.56–563.62) |

## Local filesystem cleanup

This separate fixture uses real local deletion and a file ledger, preserving a disjoint held file. Setup and correctness gates are outside the timer. Each 999-path case must use one claim/release pair; each 1,001-path case must use two. All candidates and claims must disappear.

| Paths | Before ms | After ms |
| ---: | ---: | ---: |
| 999 | 1189.85 (1068.74–1317.71) | 401.63 (387.85–409.29) |
| 1001 | 1112.95 (1062.32–1158.84) | 392.79 (360.53–402.80) |

## Validation and reproduction

The permanent `cleanup-batches` suite includes both workloads in its existing manifest registration and `benchmark all`. `--queue-counts` controls the queue-only sizes independently of `--counts` for physical cleanup. Both binaries use the same new probe functions, compiled as unoptimized all-feature library tests; CPU results are not optimized production throughput estimates. The cohorts were sequential on a shared host, so exact timing gains and a strict 20% latency ceiling are not established by these samples.

All 30 benchmark samples passed. Validation passed: 70 pack tests (two ignored probes), 25 transfer tests, 52 collection tests (one ignored probe), 235 Python benchmark tests, all-feature/all-target Clippy with warnings denied, formatting and whitespace checks.

Tests cover partial batch failure, lost ownership, cancellation of a 1,001-path cleanup, restoration of an in-flight batch and unvisited tail alongside newly published retirements, a 2,501-path all-held queue and release/retry, and existing catalog reintroduction behavior.

```sh
cargo test --locked --offline --all-features --lib --no-run -j 2
# Save the reported executable as casita-lib-test in separate before/after directories.
python3 -m benchmarks.cli all --suites cleanup-batches --repetitions 3 \
  --bin-dir /tmp/casita-queue-before-bin --output /tmp/casita-queue-before
python3 -m benchmarks.cli all --suites cleanup-batches --repetitions 3 \
  --bin-dir /tmp/casita-queue-after-bin --output /tmp/casita-queue-after
```

[Raw results, source hashes, environments and completion ledgers](2026-09-13-retirement-queue.json).
