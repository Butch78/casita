# Casita benchmark north star

Casita's benchmark program is the performance and scalability acceptance
contract for the whole repository. It is not a single speed ranking and it is
not limited to the CLI's filesystem workflow.

## Decisions the suite must support

The results page should let a maintainer answer, with raw evidence:

1. Is the operation correct under the same conditions in which it is fast?
2. Which resource becomes the bottleneck as bytes, objects, graph depth,
   fan-out, refs, history, concurrency, latency, or churn increase?
3. Is incremental work proportional to the change, or to all retained state?
4. Does peak memory remain within a deployment budget independently of total
   repository size?
5. How much physical storage and backend traffic does each logical byte or
   object amplify into?
6. What happens at p95/p99, under contention, after restart, and during
   compaction or collection?
7. Which guarantees differ between Casita and a comparator?

No composite score may hide these answers. The documentation page presents a
coverage map, operation-level measurements, scaling curves, validation status,
known cliffs, environment metadata, and links to raw results.

## Benchmark layers

The canonical suite registry is [`manifest.json`](manifest.json). It groups
work into four layers:

- **Primitives:** hashing, encoding, verification, chunking, compression,
  index lookup, and state transactions.
- **Repository workflows:** publication, filesystem import/checkout,
  transfer, collection, fsck, Casitar, and native Git.
- **Scale and operations:** large graphs, large histories, concurrency,
  network/object-store cost, compaction, restart, and cache behavior.
- **Resilience:** injected I/O failures, stale writers, process termination,
  interrupted publication, recovery, corruption detection, and ENOSPC paths.

Microbenchmarks explain a regression. End-to-end benchmarks determine whether
it matters. Both publish through the same result catalog.

## Mandatory metrics

Every timed workflow records wall, user and system time, peak resident memory,
logical input/output bytes, physical allocated/apparent bytes, and exact tool
and environment identity where applicable. Backend-aware runs additionally
record request counts, transferred bytes, range reads, retries, and cache hits.

Workloads publish semantic counters such as objects visited, objects newly
written, payloads/chunks reused, graph frontier spill, packs consulted, deltas
resolved, and refs updated. Metrics that cannot be observed are absent, never
reported as zero.

Every successful sample has a workload-specific validation gate. Examples
include byte-exact restore, closure verification, `git fsck`, expected root
CAS results, absence of leaked reachable data, or deterministic query output.

## Profiles

- **PR:** seconds; deterministic correctness plus gross regression detection.
- **Nightly:** minutes; multiple scale points and concurrency levels.
- **Release:** pinned machine and tools, warm and cold policies, balanced
  ordering, at least ten repetitions, raw samples retained.
- **Frontier:** manual cliff-finding runs such as 30+ GiB Git history, millions
  of objects, deep graphs, constrained memory, high RTT, and failure injection.

PR results are not release claims. Frontier failures are useful results and
must appear on the dashboard.

## Scale axes

At least one suite must independently sweep each axis:

- logical bytes and individual payload size;
- object count, graph depth, graph width, and duplicate ratio;
- change size relative to retained state;
- repository roots, Git refs, packs, commits, and tree entries;
- concurrent readers/writers and competing root updates;
- network RTT, bandwidth, object-store request latency, and error rate;
- memory/spill budget, cache state, and compaction generation count.

Results should retain every point rather than only the largest. The slope is
often more important than the fastest point.

## Huge repository frontier

Huge-repository coverage is a matrix, not one oversized fixture. The canonical
frontier targets live in `manifest.json` and independently exercise:

- catalog and object-store behavior projected to 500 TB of unique physical
  payload, including bounded-memory open, lookup, sync, fsck, and collection;
- at least 30 GiB of physical, incompressible repository data;
- at least 10 million reachable objects;
- at least 5 million paths in a selected tree;
- at least 1 million revisions of deep history;
- at least 10,000 incremental pack/WAL/index generations;
- a 0.001% update against a large retained repository;
- operation under a 1 GiB process-memory budget; and
- restoration of a cold serving node from authoritative remote state.

No single run needs to maximize every axis simultaneously. That would obscure
which resource caused a failure and make iteration prohibitively expensive.
Each frontier run holds the other axes stable, records the complete physical
shape, and validates the resulting repository.

Synthetic repositories provide repeatable isolated axes. Release evidence must
also include at least one real large repository supplied locally to the runner,
with its content identity recorded and sensitive names excluded from reports.
Logical generated bytes and physical packed/allocated bytes are separate
metrics. A highly delta-compressible 30 GiB logical history does not satisfy
the 30 GiB physical-data target. The 500 TB target may use a deterministic
catalog-only projection, but it must preserve the exact entry counts, shard
sizes, request amplification, and memory behavior implied by that physical
capacity; extrapolating a monolithic in-memory catalog is not a passing result.
The projection must also model incremental publication. A layout that rewrites
every digest-prefix shard touched by a small pack batch fails the request-cost
goal even if its total index bytes and point-read memory are bounded.

## Publication contract

Raw runner output is authoritative. `dashboard.py` normalizes supported runner
schemas into a catalog without deleting runner-specific fields from their raw
files. The checked-in documentation page is generated only from explicitly
selected release/frontier results. Exploratory results remain local unless
promoted deliberately.

A release-facing run records the exact Casita revision, dirty state, commands,
configuration, workload identity, tool versions, machine, kernel, filesystem,
cache policy, raw samples, failures, and aggregation method. A dirty run is
visibly marked as development evidence.

## Initial performance invariants

The dashboard should eventually enforce budgets, but the first stable
invariants are architectural:

- unchanged and incremental operations must expose total-state work versus
  delta work;
- streaming paths must show bounded memory as total bytes grow;
- remote/object-store operations must expose request amplification;
- no benchmark may trade away verification, publication atomicity, or
  comparator semantics without displaying the difference;
- failures and unsupported combinations remain visible.

Numeric regression budgets should be set only after each family has a stable
release baseline on the pinned benchmark machine.
