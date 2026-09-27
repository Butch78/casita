# Remote delta sync benchmark proposal

Status: design; the suites below are proposed, not registered runners or measured
performance claims.

The primary question is: **how long does an existing replica take to receive a
change from a remote host and publish the verified new root?** The benchmark
must distinguish work driven by the delta, the selected graph, and unrelated
retained state.

Current coverage leaves this question open. `repository` has a local
`sync-warm` delta workload. `network-scale` varies RTT and bandwidth for S3 and
atomic RPC, but each phase uses a fresh in-memory destination; its warm phase
means a warm source cache, not an existing replica.

Apply the same delta fixtures to SSH-to-local, S3-to-local, and local-to-S3
sync. Add a split-source case with `--from ssh://... --from-blobs s3://...`:
metadata comes from the SSH host while payloads come directly from its S3
replica. Prepare and validate that replica outside measurement, capture traffic
to each endpoint separately, and compare against the same source revision via
SSH alone and S3 alone. Reuse the delta/base-size axes for Git incremental fetch
over the existing smart-HTTP service as a separate Git workflow with its own
semantics and correctness gates.

## Timing and setup contract

1. Generate deterministic source revision A and sync it into a persistent
   destination. Prepare source revision B outside measurement.
2. Restore an independent copy of destination A before every measured sample.
   Repeatedly syncing into the previous sample's destination would measure a
   no-op. Use a supported consistent snapshot or a copy of closed repositories;
   never share mutable repository files through hard links.
3. Time the actual CLI pull from an SSH source on a separate host into that
   destination, from process launch through successful exit. This includes SSH
   establishment, both repository opens, discovery, transfer, receiver
   verification, and durable root publication. Imports, fixture copying, builds,
   and subsequent audits are outside the timer.
4. After timing, check the destination root against B, run integrity validation,
   and restore and compare the complete selected tree with its expected manifest.
   Verify paths, bytes, executable bits, and symlink targets. Failed validation
   invalidates the performance observation.

Run `sync --incremental` as the primary repeated-sync mode, with exhaustive
`sync` as a separately identified control. The modes have different source-audit
semantics. Never combine their results. Use local-to-local sync of the same
fixture as a diagnostic control for receiver/storage cost.

The default connection policy is a new SSH connection with connection sharing
explicitly disabled. A separate reused-SSH-connection case can reveal connection
establishment overhead; it still includes a new remote Casita process and open.
Any internal phase timing supplements the end-to-end result. Overlapping phases
must not be presented as an additive partition of elapsed time.

## Priority 1: remote-delta

Use 100,000 unique 4-KiB files in a bounded-fanout, depth-four tree. Generate
seeded incompressible content, and record actual file, directory-object, payload,
and packed-byte counts. File count is a fixture coordinate, not the count of
objects that must transfer. The source advances one named root from A to B.

| Case | Change from A to B | What it reveals |
|---|---|---|
| No-op | None | Connection, open, and closure-reuse floor |
| One item | Replace one 4-KiB file | Interactive small-update latency |
| Delta curve | Replace 10, 100, 1,000, 10,000 files | Cost per changed item and batching cliffs |
| Clustered / scattered | Same 100 replacements in nearby / widely separated directories | Ancestor rewriting and discovery sensitivity |
| Namespace-only | Separate rename, removal, and executable-bit cases for 100 paths | Metadata work without new file content |
| Additive | Add 100 unique files | Missing-object lookup and publication |
| Full transfer control | B into an empty persistent destination | Initial-transfer cost and incremental speedup |

Removed paths disappear from the selected new tree. Sync remains additive:
these samples do not expect old destination objects to be deleted or collected.

Run the delta curve at 0, 25, and 100 ms added RTT with an 8 MiB/s link budget.
Run a bandwidth sweep of 1, 8, and 64 MiB/s at fixed 25 ms added RTT for the
1,000-item case. Keep metadata/topology variants at one representative network
point initially. This avoids an expensive full Cartesian product.

## Priority 2: remote-delta-scale

Hold the delta at 10 replaced 4-KiB files while sweeping the base from 10,000 to
100,000 to 1,000,000 files. The largest point is a 0.001% file update. Include a
no-op at each size. Use the same affected subtree, fixed path depth and bounded
fanout; grow unchanged sibling subtrees and record changed ancestor sizes.

At fixed selected-tree size, independently sweep 1, 100, and 10,000 retained
historical roots/generations. Define these fixtures explicitly, keep generation
packing policy constant, and record actual packs and state/catalog bytes.
This distinguishes selected-closure discovery from repository-open, index, and
publication costs that grow with retained history.

The useful result is the slope of time, requests, discovered objects, and memory
versus base size with a constant delta. A fixed delta does not guarantee exactly
constant time: changed ancestor records, lookup depth, and publication can grow.
The benchmark should expose that growth and explain it.

## Priority 3: remote-delta-bytes

Two complementary cases separate item count from byte count:

- Hold new file content at 64 MiB: replace 16,384 files of 4 KiB, 64 files of
  1 MiB, or one file of 64 MiB. Record ancestor bytes separately. This exposes
  request/verification overhead and payload batching.
- Edit a 4-KiB region in one existing 256-MiB file; separately insert 4 KiB near
  its beginning. Measure actual transfer and destination storage growth against
  application edit bytes. Repeat with incompressible and structured content as
  separate identities. This exposes whole-payload transfer and chunk reuse.

The current SSH adapter supports bounded object/payload batches but does not
implement remote chunk negotiation. A tiny large-file edit is consequently an
essential independent case; do not infer wire savings from local deduplication
or from the number of edited bytes.

## Metrics and interpretation

Headline: end-to-end wall time per update, with all raw samples and median.
Report p95 only with at least 100 independent samples for that case; ten release
repetitions are useful for comparison but weak evidence for tail latency.

Record alongside it:

- Application changes: added/modified/removed paths and edited content bytes.
- Graph delta: newly required records and unique missing plaintext payload bytes,
  independently derived from the fixture's source and destination closures.
- Discovery and publication: records visited, records published, protocol requests
  by command, payload/chunk reuse, and spill files/bytes.
- Traffic: bytes in both directions, with protocol plaintext and encrypted SSH
  transport bytes separately labeled where observable. State the capture layer;
  an application relay does not count TCP retransmissions or packet headers.
- Resources: CPU and peak RSS on both hosts, destination allocated storage growth,
  and backend requests/bytes when a remote object store is involved.

Report wire bytes per new payload byte and records discovered per new record.
For zero denominators, retain absolute counts and mark ratios not applicable.
Missing instrumentation is missing data, never zero. Existing transfer tracing
and SSH request counters provide a starting point; full traffic and per-phase
accounting require harness/instrumentation work.

At fixed bandwidth and fixture, fit elapsed time against measured RTT to estimate
effective sequential network turns. This is a diagnostic slope, not an exact
request count: pipelining, TCP, CPU, and storage affect it. Plot delta size versus
time, fixed-delta base size versus time, and RTT versus time without collapsing
them into a composite score.

## Reproducibility and execution tiers

Use an isolated local SSH server plus controlled network shaping for repeatable
regression runs, and the same fixtures across two physical hosts for release
confirmation. Label simulation and real-host observations separately. Measure
baseline and effective RTT/throughput; 0 ms added delay is not zero real RTT.
Specify whether bandwidth is aggregate, per connection, or per direction. The
existing TCP relay shapes per connection and per direction and does not emulate
packet loss or a full WAN TCP path.

Record exact binaries/hashes on both ends, negotiated protocol capabilities,
SSH version/configuration/compression, CPU, storage, limits, cache policy, corpus
seed/hash, discovery policy, and shaping configuration. Restart application
processes for every primary sample. Distinguish warm OS caches from explicitly
conditioned cold caches on both hosts; a fresh process alone is not cold storage.
Recondition after setup and validation, which otherwise warm subsequent samples.

- Smoke: 1,000 files; no-op, one edit, and ten edits; 0/25 ms added RTT; one
  repetition. Validate the harness, not timing regression budgets.
- Nightly: the 100,000-file delta curve and fixed-delta base-size sweep, three
  exploratory repetitions. Keep the most expensive scale points configurable.
- Release: at least ten repetitions of selected cases, rotating baseline and
  candidate order on the same hosts. Run 100 repetitions of a small representative
  case when claiming p95. Preserve failed, timed-out, and skipped observations.

Set request/byte budgets from validated fixture expectations and timing regression
budgets only after establishing variance on the pinned testbed. Integrate the
runner into `manifest.json`, revision comparison, and dashboard normalization
once its versioned raw result schema is implemented.

After these three suites, add interrupted-sync retry at an observed durable
batch boundary: old root remains installed after interruption, retry reaches B,
and already committed work is reused. Record discarded work and total recovery
time. Keep this resilience case separate from uninterrupted update latency.
