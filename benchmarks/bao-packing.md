# Bao sidecar packing feasibility

`bao_packing` is a storage microbenchmark, not a production format change. It
compares fsync-enabled local object-store writes of individual Bao outboards
(up to 16 concurrent writes) with bounded immutable packs and a durable lookup
index. Pack encoding, hashing, and index publication are timed. Fixture hashing,
repository-directory creation, reopen, and verification are outside the timer.

Each pack holds at most 64 KiB including its footer and trailer. A footer row
contains the blob digest, offset, and length. The separate index maps the blob
digest to a pack digest, offset, and length. Pack names hash the complete pack.
The benchmark rejects duplicate index keys, checks footer/index agreement and
bounds, verifies pack hashes, checks every reopened outboard byte, and encodes
and verifies a range proof against each independently checked blob digest.

The permanent cases cover one and 32 small sidecars, 584/585/586 small sidecars
around the 64 KiB pack limit, and payloads immediately below/at/above 1,064,960
bytes, where outboards cross Casita's 4 KiB paging threshold. The prototype
stores raw outboards on both sides, even above that paging threshold. It does
not measure production paged outboards or their page-object writes. It also
does not measure pin admissions, catalog publication, GC, repair, overwrite,
network reads, or proof-read latency. This bounds the claim to potential local
sidecar-storage savings.

Run in the development shell:

```sh
cargo test --locked --features experimental --bench bao_packing -- --test
cargo bench --locked --features experimental --bench bao_packing
```

The target is registered in `benchmarks/manifest.json` and `benchmarks/all.py`,
so it runs in `benchmark all`. See the retained
[2026-09-19 results](reports/2026-09-19-bao-packing/README.md).

## Production implementation boundary

A production implementation needs a versioned sidecar-location table in the
same atomically published catalog as the payload objects. Keep sidecar entries
separate from chunk identities: a Bao outboard is addressed by the blob's hash,
which is not the hash of its outboard bytes. Existing chunk verification must
continue to check the actual chunk contents. Reusing the current chunk-key
namespace would break that invariant.

A narrow first implementation should pack only the existing small root objects:
inline outboards and the descriptors for paged outboards. Keep the existing
paged children and copy-on-write behavior. This avoids forcing large outboards
into a buffer or changing page verification. Proposed internal references:

- `BlobId -> (sidecar_pack_digest, offset, length, representation_version)`;
- immutable pack footer carries the same blob-to-range mapping for recovery;
- the catalog pins the exact pack generation used by readers and staged writes.

Required changes and invariants:

1. **Write and publish:** stage outboards with bounded memory, seal packs and
   fsync their data and directories before publishing the catalog reference.
   Single writes and explicit flushes must drain partial packs. Preserve staging
   pin lifetime through cancellation. Do not introduce per-sidecar mapping files.
2. **Read and compatibility:** resolve an outboard root through one accessor,
   used by full reads, range proofs, paging, and overwrite. New readers fall back
   to loose objects only when the catalog has no packed representation, never
   when a selected pack is missing or corrupt. Existing readers must reject
   unsupported catalog versions. New writers cannot silently strand old readers.
3. **GC and retention:** mark sidecar packs through live blobs and pinned catalog
   snapshots. Repack sparse live entries before publishing retirement. Deletion
   claims cover physical packs, not just the historical loose sidecar path.
   Preserve tombstones so stale loose objects cannot reappear after retirement.
4. **Repair and inventory:** fsck must enumerate both loose and packed sidecars,
   validate index/footer agreement and referenced ranges, and report corrupt or
   missing packs. Repair and overwrite publish a replacement representation
   atomically; neither may silently fall back to stale sidecars.
5. **Validation:** reopen and mixed-format stores; verified reads and tampering;
   GC with a live reader and cancelled writer; repair and overwrite; process
   death before pack sync, after pack sync, and around catalog publication;
   large sidecars staying paged; partial and full pack thresholds. Then repeat
   the real GNU grep closure comparison and retained syscall profiling.

The current feasibility index is deliberately a fresh-store index rewritten
once per sample. It is not an incremental catalog, an authenticated on-disk
format specification, or a safe replacement for Casita's repository lifecycle.
