# Local pin checkpoint investigation — 2026-09-10

The shared persistence change is on main at `218bcb1`, merged with main's newer
reader/publication cleanup changes. The merge passed 64 shared pin tests and six
retained-reader/publication integration tests on Linux. The merge changes no
checkpoint algorithm.

## Existing graph evidence

These are the separate diagnostic runs from Casita `4e7b5be`, not new untraced
runs of merged main. Inclusive phase times are accumulated and are not additive
wall time or a prediction of the gain from removing checkpoints.

| Platform / graph | Checkpoints | Checkpoint total | Checkpoint p50 / p95 | Append-sync total |
|---|---:|---:|---:|---:|
| Linux Chain-64 | 215 | 1.949 s | 8.741 / 32.780 ms | 12.812 s |
| Linux Wide-64 | 247 | 1.339 s | 3.861 / 14.841 ms | 6.510 s |
| macOS Chain-64 | 215 | 2.402 s | 11.047 / 13.336 ms | 16.197 s |
| macOS Wide-64 | 241 | 3.179 s | 12.950 / 17.113 ms | 8.440 s |

Checkpoint work matters, but append syncs have greater accumulated cost in all
four traces. The [extracted phase data](2026-09-10-checkpoints/trace-costs.json)
links each row to retained diagnostics; the
[shared persistence report](2026-09-09-shared-pin-persistence.md) links the raw
compressed traces, original commands, verification results, and hashes.

## Why checkpoints can be frequent

`flush_journal` checkpoints when either pending plus recorded operations exceeds
256, or aligned frame bytes would exceed a 1 MiB journal window. Each frame uses
disjoint 4 KiB blocks. Grouping can reduce frame and sync counts, but does not
remove the byte limit.

The incremental encoder tracks changed **records**, not changed fields. A tiny
`Protect` edit re-encodes the entire touched `DataPin`, including its unchanged
catalog and all protected resources. An oversized record therefore cannot fit
in even an empty journal window. Each successive protection edit to that record
must checkpoint the entire inventory. Re-encoding the delta precedes that
checkpoint, so its cost is in the enclosing flush phase rather than entirely in
the checkpoint phase.

The existing fourth-256-KiB-record case covers accumulated journal bytes. The
new permanent `checkpoint-record-bytes` cases isolate the single-record cliff:
1,044,480 / 1,048,576 / 1,052,672 catalog bytes, three tiny protection additions,
and an empty journal at the beginning of the measured interval. These sizes
bracket the limit; the exact payload ceiling is smaller than 1 MiB because
record/frame headers and alignment also consume space.

For the below-limit case, the expected sequence is append, checkpoint, append:
one checkpoint, two frames, four syncs. At and above 1 MiB, it is three
checkpoints, zero frames, six syncs. Setup and final cold replay are outside the
timed interval. Correctness verifies catalog bytes, every added resource,
protection cleanup, and exact durability counters.

These cases are registered through `ledger-boundaries` in `benchmarks/manifest.json`,
run in both smoke and standard profiles, and are included in `benchmark all`.

## Probe results

Both platforms passed all 48 samples, including three repetitions at each
single-record size, with identical configurations and counter results. Three tiny edits generated 3,146,326 / 3,158,994 / 3,171,282
journal/checkpoint bytes at the three respective catalog sizes. The counters
confirm a checkpoint-frequency cliff and substantial write amplification.
They do not establish a wall-time discontinuity: Linux medians for those three
edits were 42.160 / 39.628 / 41.199 ms, respectively, with host variability.
macOS medians were 39.091 / 43.479 / 51.302 ms. These small, unguarded timing
samples do not establish a portable latency cliff or a speed comparison between
platforms. The one-record fixture does not model the additional cost of
checkpointing a large unrelated live inventory.

[Linux raw results](2026-09-10-checkpoints/linux-boundaries.json) and
[macOS raw results](2026-09-10-checkpoints/macos-boundaries.json) retain all samples,
correctness gates, per-process output, environment identity, and binary hashes.
The [source record](2026-09-10-checkpoints/source.json) retains matching code hashes
and lockfile hashes. Ten benchmark-harness/registration tests also passed.

## Safety and next optimization

This investigation changes benchmarks only. The journal window, operation limit,
encoding, acknowledgment rules, checkpoint exchange, and synchronization stay
unchanged. Existing replay rejects journals beyond those limits; simply raising
a writer constant would break compatibility with existing readers. A larger
window also increases reserved capacity and recovery work.

The promising encoding improvement is to persist field-level/resource deltas
without repeatedly copying an unchanged catalog. That needs an explicit format
compatibility and migration design plus replay, crash, capacity, and GC gates.
Checkpoint synchronization cannot simply be removed: the replacement file must
be durable before atomic exchange, followed by parent-directory durability.
APFS can still fail safely at ENOSPC despite reserved file length, as documented
in the shared persistence report.

The retained graph traces do not record checkpoint trigger reasons or delta
sizes. They therefore do not establish what fraction of the 215 Chain
checkpoints comes from the oversized-record cliff, accumulated bytes, or the
operation limit. The reproducer establishes the mechanism, not its exact graph
attribution. A graph-level follow-up should record those fields and split
checkpoint encoding, allocation, write, file-sync, exchange, directory-sync,
and index-rebuild time before choosing a production optimization.

## Reproduction and scope

```sh
# Build in the platform's configured compiler environment:
cargo test --release --features cli --lib --no-run
# Use the emitted optimized test-binary path; GNU time must be on PATH:
benchmark run ledger-boundaries --no-build --probe-binary /path/to/casita-test \
  --repetitions 3 --output results/checkpoint-boundaries.json
benchmark all --suites ledger-boundaries --profile smoke --repetitions 1 \
  --output results/checkpoint-all
```

The Linux probe uses merged main `218bcb1` plus the new benchmark cases. The Mac
probe uses `4e7b5be` plus the same cases; the checkpoint and incremental encoder
implementations are identical. Linux compiled with Rust 1.97.1, Mac with 1.96.0.
The retained lockfiles and binary hashes identify the builds. These runs are
mechanism/correctness probes, not a controlled comparison of platform speed or
before/after timings: compiler, dependencies, hosts, and host activity differ.
The earlier graph timings are kept separate. No new graph performance claim is
made for merged main or the benchmark-only follow-up.
