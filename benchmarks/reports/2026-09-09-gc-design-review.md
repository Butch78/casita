# Simplify ownership of physical retirement

Review of `125ae56`, September 9, 2026. Scheduling experiments are paused; their
uncommitted harness changes and partial measurements are preserved. This is a
source review and proposed refactor. The subsequent implementation and validation
are recorded in [the implementation report](2026-09-09-publication-retirements.md).

## Recommendation

Make each catalog publication own the retirements caused by its changes. Reuse
the existing prepare/commit/abort lifecycle. Do not introduce a separate GC
journal, a public epoch API, or per-file revision numbers merely to express that
ownership. Keep ordinary GC scheduling under its caller's control for now.

The objective is a local simplification of physical cleanup. Logical reachability,
online pins, deletion claims, and crash recovery still have distinct jobs.

| Responsibility | Why it remains necessary |
| --- | --- |
| Logical roots and closure marking | Decide which objects must remain in the repository. |
| Online pins | Protect data being read or written, including historical representations. |
| Publication | Establish which catalog changes are authoritative. |
| Deletion claims | Prevent a new user from acquiring a physical path while deletion is underway. |
| Collector ownership and recovery | Settle interrupted operations without losing protection. |

These are not all duplicate GC roots. Combining them into one global lock or one
boolean would hide their different lifetimes and obstruct online work.

## Where the avoidable complexity lives

In `src/blob/pack.rs`, pending index mutations and `retired_paths` are separate.
`prepare_state_catalog` captures mutations into `PreparedIndexCatalog` through
`CatalogPreparation`. Cancellation and failed commits already restore those
mutations. Retirement paths do not travel through that lifecycle.

Consequently, `finish_collection_inner` examines shared `index_dirty` and
`prepared_index_catalog` flags before reclaiming paths. Those flags can describe
a newer writer, rather than the publication that made an older path obsolete.
The recent fix safely defers online cleanup in that case, but the flags remain
an indirect approximation of the dependency we actually need to track.

There are real gaps between recording retirement and recording its mutation:

- `retire_manifests_pinned` queues physical paths before unregistering a manifest.
- Dead-pack compaction can queue the old pack before recording its removal.
- Live-pack compaction records replacement mutations before asynchronously
  queuing the old pack and obsolete tombstone records.

Simply taking `retired_paths` when preparing a catalog is therefore insufficient:
a concurrent publication could capture a path without its removal, or capture
the removal without its path. The producer side must change too.

## Proposed lifecycle

Use one internal pending batch containing the catalog mutations and their
associated retirement candidates. Record both while holding the existing
index-to-pending lock sequence, without an await between the two updates.
Preserve the distinction between ordinary retirement and emergency deletion.

1. **Stage:** record the mutation and its retirement candidates together.
2. **Prepare:** move that exact batch into the existing preparation owner.
   New writers accumulate in the next pending batch.
3. **Commit or restore:** an acknowledged publication makes its retirements
   eligible for cleanup. A failed or cancelled preparation restores both the
   mutations and candidates, preserving the ordering of newer mutations.
4. **Reclaim:** process eligible candidates through physical liveness checks
   and deletion claims. Protected candidates stay queued; actual I/O errors
   retain the existing recovery behavior.

Publication makes a path eligible for consideration, not automatically safe to
delete. Current authoritative references and online protection still matter,
including the possibility that a content-addressed path becomes referenced
again. This is a required review/test case, not a newly demonstrated corruption
bug in the existing implementation.

The batch should wrap pending mutations rather than casually adding retirement
sets to every `IndexMutations` clone. That type is also used for background rebase
follow-ups and catalog delta construction; retirement ownership should not be
duplicated by those uses. Prepared publication already provides ownership, so a
second independently coordinated retirement transaction is unnecessary.

## Separate ready cleanup from orphan discovery

`reclaim_payloads` currently combines draining known retired paths with scanning
replacement records and unpublished payloads. The scan helpers use the mutable
catalog/index (`catalog_contains_pack`, `catalog_contains_manifest`, `location`).
A new retirement batch alone does not make that view authoritative.

First let eligible retirement cleanup progress independently of a newer writer.
Keep orphan discovery conservative while publication is pending. Only replace
that guard when the scan uses a protected committed catalog view with clearly
defined snapshot and deletion-admission semantics. Avoid making that larger scan
refactor a prerequisite for the retirement ownership improvement.

The standalone pointer-CAS publication path, `publish_current_index`, needs the
same successful-publication handoff as repository state-catalog publication.
Its failures and retries must restore the same owned batch. Background rebase
installation must not duplicate eligibility or silently drop candidates.

## Recovery and scope

Do not add persistence solely to preserve the existing in-memory optimization:
`retired_paths` is already lost on process exit. Recovery currently also uses
replacement records and orphan scans. The refactor must preserve or improve
those recovery guarantees, with crash tests for each path class; the coverage
of packs, manifests, and auxiliary records must be checked rather than assumed.

Keep the emergency ENOSPC path explicit. Its narrowly authorized deletion before
logical publication is a different contract, protected by the prune fence and
claims. Do not fold it into ordinary publication-gated retirement merely to make
the code look uniform.

The first implementation should change only retirement ownership, publication
handoff, and ready cleanup. It should remove reliance on global publication flags
for those eligible batches. It should not add adaptive scheduling, change the
public importer or reader APIs, redesign the pin ledger, or claim to replace all
GC phases with one mechanism.

## Acceptance checks

- An older committed retirement can be reclaimed while a newer writer remains
  dirty or prepared, provided current liveness and pins permit deletion.
- Retirements from that newer writer remain ineligible until its commit.
- Inject publication between mutation/retirement producer steps; the redesigned
  operation must not expose a split batch.
- Abort preparation, fail publication, then retry: pending candidates stay safe
  and eligible candidates are eventually reclaimed.
- Late and historical readers, reintroduced physical identities, failed deletion,
  and collector cancellation retain their protection and recovery guarantees.
- Verify both state-catalog and standalone-CAS publication, including background
  rebase and process crashes. Keep emergency-fence tests intact.
- Reuse the permanent 60/300-import online-holds corpus and its correctness gates
  after the refactor; do not tune scheduling at the same time.

Success means fewer independently managed states at the publication boundary,
and one explicit reason each candidate is eligible. If implementation introduces
another journal, a second publication coordinator, or a new public lifecycle,
it has exceeded this simplification's scope.
