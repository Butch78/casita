---
title: Division of Responsibility
description: Which guarantees belong to Casita, object formats, physical backends, higher-level tools, and deployments.
---

Casita is a repository backbone, not the complete product above it. Its generic
contract is strongest when each layer owns only the guarantees it can actually
enforce.

## Responsibility map

| Concern | Casita owns | Another layer owns |
|---|---|---|
| Logical identity | A namespace-qualified `ObjectKey`, canonical `ObjectRecord`, and immutable key-to-record binding | The format defines its native identifier and canonical payload |
| Relationships | Stored, verifier-produced forward links used by traversal and retention | The format decides which payload references are true retaining links |
| Publication | Payload durability, namespace verification, bounded staging, and atomic logical commit | The calling workflow decides what should be published and under which destination-owned names |
| Retention | Named roots, complete closure checks, and reachability-based collection | The application decides why a graph matters and when to repoint or remove its root |
| Transfer | One stable source revision, additive batches, receiver verification, and final root publication | Discovery, scheduling, authorization, topology, and destination policy |
| Physical storage | Complete plaintext `BlobId` contract and safe collection ordering | A `BlobStore` chooses chunking, compression, tiers, request behavior, and durability guarantees |
| Logical state | Atomic revisioned records and roots through `MetadataStore` | A deployment decides whether a backend and its backup procedures meet operational requirements |
| Git | Native immutable objects, ref views, checkout, and read-only fetch | Authoring, branches as mutable collaboration state, review, merge, push, and hosting administration |
| IPLD | Registered codec verification, CID identity, and exact links | Selectors, content routing, discovery, and network policy |
| Security | Bounds, receiver verification, safe filesystem containment, typed failure categories, and consumption of already-resolved process credentials | SecretSpec resolves deployment secrets; the deployment owns authentication, multi-tenancy, access control, confidentiality, key management, and audit policy |

For a shared physical payload service, Casita's logical collection removes only
the records in one repository. The deployment owns the aggregate cross-tenant
payload ledger, write leases, physical reclamation, and every authorization
check. See [Shared Payload Services](../shared-payload-services/).

## Trust boundaries

- **Remote senders are untrusted discovery sources.** A destination reruns its
  own namespace verifier and publishes a requested root only after its complete
  closure succeeds.
- **The logical state backend is trusted infrastructure.** Casita detects many
  inconsistencies with `fsck`, but applications should not mutate database rows
  outside the `MetadataStore` contract.
- **OpenSSH owns SSH-channel security.** Casita does not weaken host-key
  verification or replace credential and proxy configuration.
- **Stored content is not automatically confidential.** The standard local
  profile does not promise encryption at rest or access-pattern hiding.
- **SecretSpec is outside the repository boundary.** The optional S3 profile
  consumes standard AWS environment variables that `secretspec run` may
  populate. Casita does not invoke provider vaults or persist resolved values,
  and does not provide client-side encryption for that profile.
- **A valid claim is not accepted policy.** Evidence formats preserve exact
  inputs and outputs; the application decides which issuer, time window, or
  decision is trusted.

## Why the boundary matters

Moving product policy into the generic repository would make object meaning
depend on deployment state and would force unrelated formats through one
semantic model. Moving repository correctness into each application would
duplicate publication, transfer, collection, and recovery logic. The split
keeps immutable meaning format-owned and reusable lifecycle machinery generic.

Continue with [Repository](../repository/) for the concrete composition or
[Verification](../verification/) for the checks performed at each boundary.
