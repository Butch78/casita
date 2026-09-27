# Shared remote pins and checkpoint publication

Investigation baseline: `1f741ea03328690bd42b8e923f562c5f31c55190`.
This is a protocol proposal with executable experiments, not a replacement
production pin implementation. The original investigation left production
behavior unchanged; the [implementation follow-up](reports/2026-09-20-pin-batching-retries/README.md)
adds typed bounded checkpoint retries and batches known loose-chunk identities.
The independent
process experiment is a JSON/file-CAS protocol model; its bytes and timings are
not Casita codec or S3 throughput measurements. `pin-protocol-s3` runs the same
protocol prototypes over real RustFS conditional objects. The separate `remote-pin-cost`
probe executes Casita's real optimistic edit and codec. `pin-http` uses real
RustFS HTTP requests without Casita or SDK retries.

The user identified RustFS as the intended production store. The backend
reproduction therefore uses the intended engine, in an isolated local
1.0.0-rc.1 instance. A deployed cluster/version was not supplied. The full Casita
end-to-end comparison remains required before shipping a replacement protocol.
The [retained evidence](reports/2026-09-20-pin-protocol/README.md) includes all
180 local-model cases, the 60-case RustFS prototype matrix with failures,
production-code accounting, backend controls and validation logs.

## Why every writer touches the shared object

`Wal3MetadataStore::pin_store` constructs the Chroma-backed pin store at
`{prefix}/online-pins-v1`. `ObjectShardStorage` uses that same path. The key is
repository scoped, not writer scoped. A writer name identifies diagnostic WAL
ownership; it does not partition online protection. There can also be a separate
ledger under the operational `repository-holds-v1` log. Count keys separately.

The shared key atomically arbitrates writer protection, reader protection,
logical pruning, physical deletion claims, collector ownership and retired pins.
Changing only the key to include the writer name would destroy that arbitration.

| Phase | Code | Durable behavior |
|---|---|---|
| Mutation admission | `Repository::mutation_session`, `DataPinLease::try_acquire_kind` | Register a non-expiring staging pin before staging operations |
| Snapshot admission | `pin_metadata_snapshot_kind` | Read candidate, register its generation/catalog/resources, reread and compare revision/resources before exposing it; discard and repeat if changed |
| Metadata shard reads | `Wal3MetadataStore::pin_state_shards` | Register referenced metadata paths before reading, validate the manifest; not needed for an empty path set |
| Payload and physical writes | `WritePins::run`, `DataPinLease::protect` | Confirm resource protection before storage I/O; tracked tasks retain ownership through cancellation |
| Publication | `MutationSession::publish_inner_with_metadata` | Protect staged objects, links and new root targets, acquire a fresh pinned snapshot, check root expectations, verify closure, then commit |
| Refresh or “renewal” | Snapshot/manifest admission loops | New admission and later release, not heartbeat renewal. Remote pins have no TTL and no periodic renewal task |
| Release | `OwnedPin::drop`, `MemoryPinStore::release` | Queue tracked release. Remove normally; during collection retain the pin in the retired set until the collector finishes |

Relevant implementations:
[remote persistence](../crates/casita/src/metadata/pins/persistent.rs),
[lease lifetime and coalescing](../crates/casita/src/metadata/pins/runtime.rs),
[pin state transitions](../crates/casita/src/metadata/pins.rs),
[repository publication](../crates/casita/src/repository/mutation.rs),
[WAL commit](../crates/casita/src/metadata/wal3.rs), and
[shard maintenance barrier](../crates/casita/src/metadata/wal3_shard.rs).

`attempt_edit` loads and decodes the **entire** inventory, applies the operation
through `MemoryPinStore`, copies the resulting inventory, encodes/checksums it,
and conditionally replaces the entire remote object. A lost CAS repeats the
whole sequence against a fresh GET. There is no unconditional PUT fallback.
Chroma's queue serializes one load/apply/PUT attempt per storage-client pointer
and path. It releases the queue during conflict backoff. Independent processes
have independent queues, so they still contend at the shared object.

`WritePins::protect` also iterates every captured owner. The normal mutation
write scope selects its own staging lease, but unscoped operations on a shared
physical backend can conservatively capture multiple active owners. Thus even
one physical upload need not correspond to just one ledger protection edit.
Record captured owner count when attributing a publication's requests.

### Request accounting

These are logical storage calls, excluding SDK/network retries:

* Successful state-changing registration, protection, release or claim: one
  whole-ledger GET and one whole-ledger conditional PUT.
* Refused or unchanged operation reaching the store: one GET, zero PUTs.
* A repeated protection satisfied by `DataPinLease::confirmed`: zero requests.
* Each failed conditional PUT: one additional GET and attempted whole PUT.
* Explicit inventory inspection: one additional whole GET.

Let A be registrations, U actual durable protection batches, L changed releases,
G other successful collector/claim edits, N unchanged/refused store operations,
I inventory reads, and C failed conditional attempts. Across a publication's
complete lifetime, `GET = A + U + L + G + N + I + C` and
`PUT attempts = A + U + L + G + C`. Charge only that publication's operations
to its counters; report concurrent GC separately. Errors before PUT and SDK
retries require additional counters, not assumptions about this equation.

For a fresh single staging pin and R distinct sequential protection calls,
registration through release costs `R + 2` GET/PUT pairs. Grouping known
resources in batches of B costs `ceil(R/B) + 2`. At R=64 and B=8 this is
66 versus 10 pairs, a 84.85% reduction in this isolated path. One hundred such
pins require 6,600 versus 1,000 pairs without conflicts. This is not a claim
that an entire Casita publication uses only those pairs: snapshot acquisition,
metadata pins, catalog/pack uploads, retries and physical layout add work.
There is no single fixed whole-publication count independent of those inputs.

The permanent production-code probe checks these exact counts for 1/10/32/64/100
active pins, 7/8/9/64 resources, and batches of 1/8. Its active-pin dimension is
in one process, and is deliberately distinct from the process-model matrix.

If W is the active pin count and P the total retained resource count, an edit
transfers and processes `Theta(W + P + catalog bytes + deletion/retired state)`.
For a simple CASPIN03 staging-only inventory, the wire size is 62 bytes of
fixed framing plus 38 bytes per pin plus each resource. A storage path consumes
5 bytes plus UTF-8 path length. Blobs/chunks consume 33 bytes each. Inline
catalogs can dominate these terms. A growing pin with R individually added
resources incurs quadratic cumulative ledger bytes even without contention.
If W writers each retain R resources while making proportional progress, all
writers together can produce `Theta(W^2 R^2)` cumulative resource bytes in this
simple growing workload. Actual lifetime overlap and release order matter.
Contention adds another multiplier: if W processes read the same version for
one edit each and retry in synchronized rounds, only one wins each round and
the burst takes `W(W+1)/2` attempted GET/PUT pairs for W successful edits.
Real scheduling/backoff changes that count; the attempt limit can turn the
extra work into failed admissions or publications instead of eventual success.

The codec rejects inventories over 64 MiB without removing protection. Stuck
writers and retired pins can therefore create both a bandwidth cost and a hard
availability limit. The process model does not claim to exercise this real
codec limit. Existing codec/ledger boundary tests remain necessary.

The reported 59/60 seconds of queue occupancy means approximately 98.3% busy
time for that run. It is consistent with serialized remote round trips and
whole-ledger processing, not proof of CPU saturation. The original trace was
not available here. Instrument queue wait separately from held time, GET/PUT
latency, codec CPU, byte counts and CAS failures. The current retry policy has
32 attempts with 1/2/4/8/16/32/64 ms capped backoff and no jitter: up to 1.727 s
of sleeps plus network time. It is not a wall-clock deadline.

## Safe batching

This baseline already batches pending protection requests **within a lease**.
`try_protect` queues weak references, elects a leader with `protection_gate`,
unions live pending requests, and confirms only after durable success. Repeated
confirmed identities avoid the ledger entirely. Refused resources are tracked
separately so one claim-conflicting request does not repeatedly block unrelated
waiters. The small-blob path also combines blob/chunk identity protection.
Consequently an unbatched reference is a request-shape control, not a description
of every execution of today's importer.

The next batching step should combine simultaneously known identities before
starting dependent I/O. Bound by encoded bytes as well as count. A useful
experiment is 8 resources per flush, including 7/8/9-resource boundary cases;
this is an experiment parameter, not a selected production optimum.

| Update | Earliest safe combination and required durability |
|---|---|
| Initial known resources | Include in registration; registration must be durable before any dependent read/reuse/upload |
| Blob/chunk/path IDs known together | Union in one protection edit before **any** corresponding upload; do not wait until after a pack is visible |
| Concurrent requests on one lease | Existing coalescer; each waiter is acknowledged only if its own entire set is confirmed |
| Publication inputs | Union objects, links and targets before closure reads and root commit; preserve snapshot validation after pin admission |
| Physical pack/catalog/shard locations discovered later | Separate barrier when identities become known; cannot be retroactively covered by an earlier batch |
| Releases | May delay and batch exact-token releases after all owned I/O settles; overprotection is safe, early release is not |
| Cross-lease ledger edits | Possible ordered group commit under one CAS, but needs per-operation outcomes, stable retry identities and cancellation ownership; not implemented by the existing remote queue |

Never batch across an unfulfilled durability dependency. Cancellation before
admission can discard a weak pending request; cancellation after launching a
durable request must leave a tracked owner until it settles. A lost success
response must not populate the confirmed cache. Retrying a union is idempotent
for a known live token, but an uncertain registration can orphan a token because
the current memory implementation generates a fresh token during apply. Such
orphan protection needs explicit recovery, not expiry. A batch containing a
claimed resource must refuse atomically or return precise per-request results;
it must not falsely acknowledge its unrelated waiters.

## Writer-owned records with a collection barrier

Use a separate versioned protocol namespace. Required storage properties are
linearizable GET and create/replace-if-version for each key. Do not infer these
properties from the name “S3 compatible.” GC must have complete discovery of
admitted writers without treating object listing as a transactional snapshot.

Proposed objects:

* A small CAS registry: protocol version, monotonic epoch, phase, collector
  identity and admitted unique writer incarnations. Readers that retain data
  need the same discovery contract. A runner name is not an incarnation.
* One record per incarnation: monotonic sequence, active/frozen/terminal state,
  collector epoch/token when frozen, and immutable resource references or an
  append-only protection-set root. Initially use full per-writer sets; chunked
  manifests require their own protection/reachability proof.
* A durable collection plan: frozen record versions, metadata root witness,
  deletion candidates/claims, phase and completion receipts. Publication
  reconciliation uses a durable operation identity in the metadata log.

In the real implementation, each writer record must preserve per-operation
tokens, pin scopes, generations and catalog witnesses, not flatten everything
into physical paths. A process can have multiple overlapping leases. Releasing
one lease must not remove a resource still owned by another. The model's single
operation per writer does not exercise that local ownership bookkeeping.

Normal admission and writing:

1. Create a unique empty writer record conditionally. No storage operations
   become authorized merely by creating it.
2. CAS that identity into the registry only while admission is open. GC closes
   admission using CAS on this **same object**. If admission loses, the writer
   retries after reopening and must not upload or reuse data in the meantime.
3. Serialize updates of this writer record locally. Persist resource additions
   by CAS while its state is active. Only after confirmation may dependent I/O
   start. Writer records need monotonic versions to prevent ABA across freeze
   and thaw, even when their resource set is unchanged.
4. Publish only verified closures covered by durable protection. Keep protection
   until commit outcome is known and all related I/O has settled. Publication
   during GC may use already protected resources; it must not introduce an
   unprotected dependency. A conservative initial implementation can simply
   wait for GC to finish.

Collection:

1. Acquire the existing non-expiring collector authority. CAS registry admission
   closed with a new epoch and collector token; retain the exact member list.
   An admission serialized before this CAS is in the list. One after it fails.
   A created-but-unadmitted orphan has no authority to write anything.
2. CAS each listed writer record from active to frozen by this epoch/token,
   preserving its complete resource set. If an addition wins first, reload and
   include it. If the freeze wins first, the old addition's CAS must fail before
   its caller uploads. A barrier read followed by an unconditional writer PUT
   is insufficient and is specifically forbidden.
3. Do not delete until **every** admitted record is frozen. An unavailable or
   undecodable record aborts collection conservatively. A stopped writer does
   not block the freeze itself; it just contributes its last durable protection.
4. Read a fresh metadata root witness after freezing. Mark the union of its
   reachable graph and all frozen protection. Releases cannot erase frozen
   sets. An in-flight publication is covered either by the fresh roots or by
   its retained frozen resources, including uploads not completed at mark time.
   Use collector-owned read authority for this witness and its shards; ordinary
   reader admission would deadlock against the collector's closed registry.
5. Persist exact deletion claims/plan before issuing deletes. Protect the plan's
   own storage and retain claims until all potentially delayed deletes settle.
   Registry admission remains closed while those deletions can still arrive.
   Collector replacement uploads use collector-owned durable protection before
   I/O, and cannot be added to the already fixed deletion candidate set. Do not
   make GC acquire an ordinary writer admission that its own barrier blocks.
6. Complete sweep and catalog publication, durably record completion, thaw
   records with the same epoch/token CAS, and finally reopen the registry.
   Never force-open a closed phase because a deadline elapsed.

This first version pauses new protections and new admissions during sweep. It
removes the global CAS from ordinary resource additions, not from writer
admission or collection. Registry churn remains O(W) bytes per registration in
the simplest implementation; keep one incarnation per process rather than per
object. Per-writer additions copy O(Pw), not O(sum Pw). GC now needs O(W) reads
and freezes, plus O(sum Pw) scanning. Benchmark pause duration and collector
requests alongside writer request savings. Sharded registries or online sweep
with range/epoch claims are separate extensions, not assumed optimizations.
Admission fairness also matters: the RustFS prototype can exhaust its admission
budget when repeated collections reopen the registry for only 10 ms. Private
records do not fix that starvation. A production scheduler needs a measured
admission opportunity between passes and bounded collector duty cycle; reducing
the GC frequency is a liveness policy, never permission to expire protection.
This does not partition the metadata WAL or remove root-publication contention;
after fixing pins the WAL may become the next bottleneck. The model uses
distinct per-writer root keys to isolate protection costs.

### Crash and ambiguous-write recovery

Pins never expire. A heartbeat helps operators locate a writer; missing it
does not authorize reclamation. A crashed writer's frozen resources remain in
the mark set. Removing them requires evidence that the exact incarnation is
stopped and cannot resume, and that outstanding operations have settled or
have been fenced at every authority they could still modify. Revoking a pin
alone cannot fence a late root publication.

Never delete/recreate mutable coordination keys to “reset” a version. Keep
terminal tombstones, use fresh incarnation identifiers, and fence stale CAS
requests with a new monotonic sequence. A successful read of absence after a
timeout does not prove an earlier request cannot arrive later. Resolve an
uncertain admission with a membership observation or a subsequent conditional
fence on the same registry. Resolve an uncertain resource union by reading its
membership under the current record version; preserve it if resolution fails.

Registry compaction must retain enough terminal identities/epochs to reject
delayed admissions and must not reintroduce an old record namespace. The
executable model retains empty writer records and does not implement compaction.
Production capacity tests must include long-lived history, not just active W.

Concretely, normal shutdown can CAS an incarnation to an immutable terminal
tombstone after its leases and storage work have settled and its publications
are resolved. Recovery can do the same only after establishing the fencing
conditions above. While admission is closed, GC may remove such terminal
members from the registry, keeping their versioned tombstones outside the
active scan. An old incarnation may never be admitted again or changed back to
active. This bounds registry work by admitted owners without deleting the
object that rejects delayed conditional writes. Deleting terminal records
requires the stronger proof that no old request can recreate them or produce
ABA; a new registry epoch alone is insufficient to establish that.

An interrupted collector leaves admission closed and the plan durable. Recovery
must establish that the collector cannot resume, reconcile outstanding deletes,
then resume its exact plan or conservatively remark under new fenced authority.
Changing a coordination token does not cancel an already issued object DELETE.

Upgrade requires draining/fencing old-protocol writers and readers, reconciling
legacy pins/claims, and publishing a version capability fence that old clients
cannot ignore. Reading both ledgers without coordinating admissions is unsafe.
The existing forceable metadata-shard maintenance barrier is not, by itself,
the writer-record collection barrier proposed here.

## RustFS five-second failures

`pin-http` runs independent processes with raw signed HTTP requests, no Casita,
and no SDK retries. It separates disjoint PUT, unconditional hot-key PUT, and
GET plus If-Match hot-key CAS. CAS values retain unique operation receipts so a
fresh uncached final GET can check every acknowledged receipt. Unconditional
controls intentionally use identical bytes and verify acknowledged contents
after all clients exit. Each HTTP event retains method, status, error code,
latency and request ID; newer runs also retain the error body.

RustFS `1.0.0-rc.1`, source tag commit
`dfdea99af7a2765446592a424a95f5fdeae99980`, reproduced five-second HTTP 503
`ServiceUnavailable` responses in hot-key unconditional PUTs. Disjoint PUTs
did not produce 503s in the initial 1/10/32/64/100 matrix. Hot-key CAS produced
412 `PreconditionFailed` responses. A separate 32-client control changing
`RUSTFS_OBJECT_LOCK_ACQUIRE_TIMEOUT` to 2 seconds moved the 503s to about two
seconds. This is a causal timeout-setting control, not just a latency guess.

Pinned source supports that attribution: the
[object lock timeout defaults to five seconds](https://github.com/rustfs/rustfs/blob/dfdea99af7a2765446592a424a95f5fdeae99980/crates/config/src/constants/object.rs#L362),
[PUT acquires namespace write locks](https://github.com/rustfs/rustfs/blob/dfdea99af7a2765446592a424a95f5fdeae99980/crates/ecstore/src/set_disk/ops/object.rs#L990),
and [storage lock errors map to ServiceUnavailable](https://github.com/rustfs/rustfs/blob/dfdea99af7a2765446592a424a95f5fdeae99980/rustfs/src/error.rs#L297).
Disk-permit and quorum failures have other paths; status 503 alone is not a
unique diagnosis. The controls do not establish the internal reason the lock
holder was slow, nor explain every 503 in a previous run using a different build.
The 100-writer protocol matrix also observed five-second 503s on ledger GETs.
[GET acquires a namespace read lock](https://github.com/rustfs/rustfs/blob/dfdea99af7a2765446592a424a95f5fdeae99980/crates/ecstore/src/set_disk/ops/object.rs#L345),
so read-side lock contention is a plausible explanation, but the timeout-setting
control above was performed on PUTs. These backend failures are recorded
separately from the earlier 32/64-writer conditional-conflict and admission
failures; no blanket retry of ambiguous writes is inferred from a 503 code.

The evidence supports a RustFS hot-object lock acquisition failure in this
reproduction, separate from Casita's optimistic-CAS failure handling. It does
not establish an S3-wide limitation. Repeat the same controls and the actual
Casita workload on the deployed RustFS configuration with bucket region, conditional-write
support, endpoint version/configuration, connection limits and SDK retry settings
recorded. Do not benchmark through a production data prefix. The runner creates
a unique child prefix in a caller-specified dedicated test location.

## Checkpoint retry classification

The exact message `logical shard checkpoint publication is fenced by maintenance`
comes from `ObjectShardStorage::acquire_barrier`: a checkpoint sees a non-idle
barrier before acquiring it. The investigation baseline returned
`MetadataError::Transient`; the implementation follow-up returns the typed
`MetadataError::MaintenanceFenced`. It can be
another checkpoint as well as GC. This invocation has not attempted its WAL
append yet. `commit_loaded` reaches this path when it needs a checkpoint:
more than eight tail deltas, a cumulative delta record larger than 1 MiB, or
collection's empty tail. Cover 8/9 deltas and below/at/above 1 MiB in actual
publication tests, not just ordinary delta commits.

The similar message `logical checkpoint publication was fenced by maintenance`
occurs after append contention/reconciliation when barrier ownership changed.
These two messages must not be treated as one retry class. In particular,
`reconcile_contention` also handles indeterminate/durable outcomes. It recognizes
the proposed revision when that is the current head; if a later revision exists,
head mismatch alone does not prove the earlier commit never happened.

At the investigation baseline, the repository publication loop retried `StaleRevision` unless an exact
revision was requested. It rereads snapshots and rechecks root expectations,
but that outer stale loop was unbounded. It returned other errors, including the
maintenance `Transient`, directly. The follow-up bounds stale and typed
pre-append maintenance retries together at 32 attempts and 30 seconds. The WAL
append loop itself is also bounded. Do not solve other failures with
`retry every Transient` or string matching.

| Outcome | Handling and remaining work |
|---|---|
| Maintenance admission refused before append | Safe bounded retry while keeping original staging ownership; reacquire snapshot/barrier and rebuild the candidate |
| Explicitly rejected CAS/append with proof of noncommit | Retry within shared attempt/time budget, rereading root expectations; exact-revision requests still return stale |
| Root expectation changed | Return root mismatch; do not overwrite the winner |
| Checkpoint fenced after possible append | Reconcile exact operation identity before replay; a changed fence is not proof of noncommit |
| Timeout/reset/503 after sending commit | Unknown outcome until stable log/receipt reconciliation or a definitive storage fence proves otherwise |
| Commit acknowledged but barrier cleanup failed | Preserve success; cleanup/recovery is separate, as current code already does |
| Validation, corruption, missing conditional-write support, capacity error | Return failure; retries do not repair these conditions |

The follow-up introduced the typed pre-append reason and retains the staging
pin across its retry loop. A typed indeterminate outcome and durable operation
identity remain to be implemented. Keep the prepared verified records and a
strong staging pin in an internal publication attempt object so retries do not
restage or drop ownership.
Keep the prepared payload catalog too: `Publication::commit` currently calls
`prepared.abort()` on every metadata error, while `PreparedCatalog`'s contract
requires keeping it until the outcome is known. An unknown outcome must be
resolved or durably handed to recovery before treating that preparation as
rejected. This needs a fault test; abort restores unpublished changes and is not
itself evidence that a remotely submitted root commit cannot still succeed.
The follow-up uses one attempt budget and wall-clock deadline for publication's
stale and maintenance retry loop, with jittered backoff. The 32-attempt,
30-second policy is not a correctness condition or pin TTL. Separate backend
retry policies remain unchanged.
If a deadline cancels the caller, shield outstanding storage tasks and retain
their pins until they settle. Return a resumable/reconcilable attempt identity
when outcome remains unknown. Do not spend a fresh 32-attempt budget at every
layer.

Each retry must obtain a fresh pinned snapshot, reevaluate the caller's original
root expectations, recompute any candidate that depended on the previous root,
and acquire/validate the current checkpoint barrier. Never silently update the
caller expectation to the winner's value. Durable receipts or WAL history must
distinguish “my commit happened, then another happened” from “mine never
committed”; observing only the current root cannot do that generally.

## Permanent experiments and remaining gates

All four new entrypoints are registered in `manifest.json` and `benchmark all`:

```sh
benchmark run remote-pin-cost
benchmark run pin-protocol --profile smoke --repetitions 1 \
  --output /tmp/pin-model.json --report /tmp/pin-model.md
benchmark run pin-protocol-s3 --profile smoke --output /tmp/pin-protocol-s3.json
benchmark run pin-http --profile smoke --output /tmp/pin-http.json
benchmark run pin-http --writers 32 --operations 8 --modes hot-put \
  --lock-timeout-seconds 2 --output /tmp/pin-http-lock2.json
# To repeat against a dedicated prefix on the deployed RustFS cluster:
benchmark run pin-http --endpoint https://ENDPOINT --region REGION \
  --bucket TEST_BUCKET --prefix TEST_PREFIX --profile standard \
  --repetitions 3 --output /tmp/pin-http-production.json
```

Run in the pinned development shell. Production HTTP credentials are read from
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optional `AWS_SESSION_TOKEN`;
do not put them in command arguments or reports. The HTTP runner requires a new
artifact directory and retains its isolated RustFS server log and data there.

`pin-protocol` starts 1/10/32/64/100 independent spawned writer processes and a
concurrent collector for every design. It covers 7/8/9 resources around batch 8,
plus 64 in the standard profile. Cases are normal operation, SIGKILL after a
durable first batch/upload, settled writes with lost responses, and cancellation
after the first upload. A fresh reader process checks the complete bytes and
identities of every acknowledged output after collection. Crash cases assert
protection survives GC, then prove the owner has exited before releasing its
exact record and asserting eventual reclamation. Tests additionally schedule
stale admission and stale protection CAS attempts across the barrier.

`pin-protocol-s3` substitutes real RustFS GET, conditional PUT, paginated LIST
and DELETE for the local conditional-file fixture and reuses those process,
crash, cancellation and fresh-readback gates. Smoke uses nine objects per writer;
standard includes 7/8/9/64. Conditional 409/412 conflicts are counted separately
from 503s. Other backend failures terminate the sample, never authorizing
discarding protection. A failed GET can be retried as a read in a production
client; it does not itself create an ambiguous commit. This fixture has no SDK
retry layer. Fault injection hides an actual successful
PUT response from the protocol, then verifies idempotent reconciliation. The
records and root format remain research prototypes, not Casita WAL publication.
The supervisor retains failed cases and continues the matrix. After quiescing
an aborted fixture it starts a new reader to enumerate and validate every
durable root, including publications whose acknowledgment queue was interrupted.
Passing this audit does not turn a failed liveness sample into a successful
performance sample or prove settlement of arbitrary delayed network requests.

Both protocol fixtures run GC continuously, sleeping 10 ms after each complete
pass. This is an aggressive collection stress workload, not an assumed
production GC cadence. The prototype's shared freeze and writer-owned freeze
both cause polling while closed. Report those GETs rather than claiming that
private records remove every shared cost. The local matrix completed all 180
cases with 7,362 acknowledged outputs passing fresh readback. The RustFS matrix
also records bounded-edit failures; consult the retained report for its status.

Model limits are deliberate: shared GC conservatively closes all mutation
admission rather than reproducing Casita's fine-grained claims/retired pins;
there is one collector and no collector takeover;
stable model owner IDs simplify ambiguous registration; ambiguous writes are
settled successes with lost replies, not arbitrary network histories; publication
is a simple immutable-object root, not Casita's WAL/catalog/pack protocol; local
file-CAS timing has no remote RTT, and its jittered 60-second edit bound differs
from the production 32-attempt policy. These results cannot prove production crash
safety or the performance of a shipped writer-owned implementation.

Before shipping, retain an actual Casita 1/10/32/64/100-process matrix for all
three implementations on the intended store. Add deterministic pauses around
protect/upload, freeze/mark, claim/delete, root append/acknowledgment and release;
kill writers and collectors at each boundary; inject committed-but-lost and
delayed/uncommitted responses; test cancelled coalescing leaders and refused
batch members, shared deduplicated resources, overlapping local leases and
competing updates of the same named root; exercise checkpoint thresholds and later commits during
reconciliation. Fresh-process readback of every acknowledged named output,
staging survival and eventual exact-owner reclamation are acceptance gates.
Report throughput, p50/p95/p99, queue wait/occupancy, requests/bytes by key and
operation, 409/412 conflicts, raw 503s, SDK retries, GC pauses and recovery leaks.

My next implementation preference is bounded typed pre-append retries and
durability-preserving batching first, then a writer-owned prototype behind a
protocol-version fence. The alternative is to prioritize writer-owned records
immediately if the production endpoint confirms the shared hot key is the
dominant limit. The current experiments alone do not select that alternative.
