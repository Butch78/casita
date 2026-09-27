//! Exact mutations and the pack catalog v1 root format.
//!
//! Mutation encoding consumes the IDs recorded by the write path. It must not
//! discover changes by comparing two complete catalogs: that would make every
//! small publication O(repository size). The root supports an inline base for
//! small repositories, an immutable checkpoint for intermediate repositories,
//! and an immutable shard map for repositories whose catalog is larger than a
//! single object or the process memory budget.

use std::collections::BTreeMap;

use super::*;

pub(super) const INDEX_DELTA_MAGIC_V1: [u8; 8] = *b"casitid1";
pub(super) const INDEX_CATALOG_MAGIC_V1: [u8; 8] = *b"casitap1";
pub(super) const INDEX_CATALOG_MAGIC_V2: [u8; 8] = *b"casitap2";
pub(super) const DELTA_INVENTORY: &[u8] = b"casita index delta patch v1\0";
// Encoded bytes, not an arbitrarily short mutation count, are the primary
// sealing limit. The count remains a defensive CPU bound for tiny deltas.
const MAX_INLINE_DELTAS: usize = 1024;
pub(super) const MAX_INLINE_DELTA_BYTES: usize = 1024 * 1024;
const MAX_SHARD_BITS: u8 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CatalogBase {
    /// The complete checkpoint is carried in `pack-index-current`; open costs
    /// one GET and is preferred while the checkpoint remains small.
    Inline(Bytes),
    /// A complete immutable checkpoint stored under its BLAKE3 digest.
    Checkpoint(Digest),
    /// An immutable shard-map object stored under its BLAKE3 digest. The map
    /// describes the chunk, manifest, and pack-state shard tables.
    Sharded { root: Digest, shard_bits: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogRunQueryRef {
    pub(super) offset: u64,
    pub(super) encoded_bytes: u64,
    pub(super) digest: Digest,
    /// Authenticated block routing and exact changed-pack membership. This is
    /// carried by the WAL3 catalog root, so point lookup needs only one range
    /// GET for the selected block.
    pub(super) routing: Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogRunRef {
    pub(super) digest: Digest,
    pub(super) first_generation: u64,
    pub(super) last_generation: u64,
    pub(super) encoded_bytes: u64,
    pub(super) query: Option<CatalogRunQueryRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeltaCatalog {
    pub(super) sidecars: Option<Digest>,
    pub(super) generation: u64,
    pub(super) base: CatalogBase,
    /// Immutable binary-leveled deltas keyed by level.
    pub(super) runs: BTreeMap<u8, CatalogRunRef>,
    /// The newest small deltas, retained inline until they form one run.
    pub(super) deltas: Vec<Bytes>,
}

/// Exact IDs changed since the catalog root observed by this writer.
///
/// The owner must capture this value and the corresponding [`Index`] snapshot
/// atomically. Recording a pack is sufficient even when only its tombstone
/// state changed: encoding reads the final state for that pack from the
/// captured snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct IndexMutations {
    changed_packs: HashSet<PackId>,
    added_manifests: HashSet<BlobId>,
    removed_manifests: HashSet<BlobId>,
    manifests_complete: bool,
}

/// A validated catalog delta whose negative operations remain explicit.
///
/// Eager catalogs can apply this directly to an [`Index`]. Sharded catalogs
/// must retain `removed_packs` and `removed_manifests` as an overlay because
/// the referenced records may live in base shards that have not been fetched.
#[derive(Clone)]
pub(super) struct DecodedIndexDelta {
    pub(super) removed_packs: Vec<PackId>,
    pub(super) removed_manifests: HashSet<BlobId>,
    pub(super) patch: Index,
}

impl IndexMutations {
    pub(super) fn record_pack(&mut self, pack: PackId) {
        self.changed_packs.insert(pack);
    }

    pub(super) fn record_manifest_add(&mut self, manifest: BlobId) {
        self.removed_manifests.remove(&manifest);
        self.added_manifests.insert(manifest);
    }

    pub(super) fn record_manifest_remove(&mut self, manifest: BlobId) {
        self.added_manifests.remove(&manifest);
        self.removed_manifests.insert(manifest);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.changed_packs.is_empty()
            && self.added_manifests.is_empty()
            && self.removed_manifests.is_empty()
            && !self.manifests_complete
    }

    /// Restore an older unpublished batch ahead of mutations recorded while a
    /// publication was in flight. Newer manifest operations win.
    pub(super) fn prepend(&mut self, older: Self) {
        let newer = std::mem::take(self);
        *self = older;
        self.changed_packs.extend(newer.changed_packs);
        for manifest in newer.added_manifests {
            self.record_manifest_add(manifest);
        }
        for manifest in newer.removed_manifests {
            self.record_manifest_remove(manifest);
        }
        self.manifests_complete |= newer.manifests_complete;
    }
}

#[cfg(test)]
fn same_pack_state(left: &Index, right: &Index, pack: PackId) -> bool {
    left.packs.get(&pack) == right.packs.get(&pack)
        && left.pack_lengths.get(&pack) == right.pack_lengths.get(&pack)
        && left.tombstoned.get(&pack) == right.tombstoned.get(&pack)
        && left.tombstone_records.get(&pack) == right.tombstone_records.get(&pack)
}

#[cfg(test)]
pub(super) fn encode_index_delta(base: &Index, next: &Index) -> io::Result<Bytes> {
    let mut mutations = IndexMutations::default();
    mutations.changed_packs.extend(
        base.packs
            .keys()
            .chain(next.packs.keys())
            .filter(|pack| !same_pack_state(base, next, **pack))
            .copied(),
    );
    mutations
        .changed_packs
        .extend(next.superseded.difference(&base.superseded).copied());
    let base_manifests = base.manifests.sorted_ids();
    let next_manifests = next.manifests.sorted_ids();
    mutations.added_manifests.extend(
        next_manifests
            .iter()
            .filter(|manifest| base_manifests.binary_search(manifest).is_err())
            .copied(),
    );
    mutations.removed_manifests.extend(
        base_manifests
            .iter()
            .filter(|manifest| next_manifests.binary_search(manifest).is_err())
            .copied(),
    );
    mutations.manifests_complete = next.manifests_complete && !base.manifests_complete;
    encode_index_mutations(next, &mutations)
}

pub(super) fn encode_index_mutations(
    next: &Index,
    mutations: &IndexMutations,
) -> io::Result<Bytes> {
    let mut removed = mutations
        .changed_packs
        .iter()
        .filter(|pack| !next.packs.contains_key(pack))
        .copied()
        .collect::<Vec<_>>();
    removed.sort_unstable();

    let mut changed = mutations
        .changed_packs
        .iter()
        .filter(|pack| next.packs.contains_key(pack))
        .copied()
        .collect::<Vec<_>>();
    changed.sort_unstable();

    let mut superseded = mutations
        .changed_packs
        .iter()
        .filter(|pack| next.superseded.contains(pack))
        .copied()
        .collect::<Vec<_>>();
    superseded.sort_unstable();

    let mut removed_manifests = mutations
        .removed_manifests
        .iter()
        .copied()
        .collect::<Vec<_>>();
    removed_manifests.sort_unstable();
    let mut added_manifests = mutations
        .added_manifests
        .iter()
        .copied()
        .collect::<Vec<_>>();
    added_manifests.sort_unstable();

    let mut patch = Index::default();
    for pack in changed {
        let entries = next
            .packs
            .get(&pack)
            .expect("changed pack remains present")
            .clone();
        let pack_len = next
            .pack_lengths
            .get(&pack)
            .copied()
            .ok_or_else(|| io::Error::other("changed pack has no length"))?;
        patch.add_pack_metadata(pack, pack_len, entries);
        if let Some(dead) = next.tombstoned.get(&pack) {
            patch.tombstoned.insert(pack, dead.clone());
        }
        if let Some(records) = next.tombstone_records.get(&pack) {
            patch.tombstone_records.insert(pack, records.clone());
        }
    }
    patch.superseded.extend(superseded);
    patch.manifests = ManifestIndex::from_sorted(added_manifests);
    patch.manifests_complete = mutations.manifests_complete;

    encode_decoded_index_delta(&DecodedIndexDelta {
        removed_packs: removed,
        removed_manifests: removed_manifests.into_iter().collect(),
        patch,
    })
}

pub(super) fn encode_decoded_index_delta(delta: &DecodedIndexDelta) -> io::Result<Bytes> {
    let mut removed = delta.removed_packs.clone();
    removed.sort_unstable();
    removed.dedup();
    let mut removed_manifests = delta.removed_manifests.iter().copied().collect::<Vec<_>>();
    removed_manifests.sort_unstable();
    if removed_manifests
        .iter()
        .any(|manifest| delta.patch.manifests.contains(manifest))
    {
        return Err(io::Error::other(
            "catalog delta both adds and removes a manifest",
        ));
    }
    let patch = encode_index_checkpoint(&delta.patch, Digest::from(blake3::hash(DELTA_INVENTORY)))?;
    let mut bytes = Vec::with_capacity(
        8 + 24 + (removed.len() + removed_manifests.len()) * DIGEST_LEN + patch.len(),
    );
    bytes.extend_from_slice(&INDEX_DELTA_MAGIC_V1);
    bytes.extend_from_slice(&(removed.len() as u64).to_le_bytes());
    for pack in removed {
        bytes.extend_from_slice(pack.as_digest().as_bytes());
    }
    bytes.extend_from_slice(&(removed_manifests.len() as u64).to_le_bytes());
    for manifest in removed_manifests {
        bytes.extend_from_slice(manifest.as_digest().as_bytes());
    }
    bytes.extend_from_slice(&(patch.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&patch);
    Ok(bytes.into())
}

pub(super) fn decode_index_delta(bytes: &[u8]) -> io::Result<DecodedIndexDelta> {
    if bytes.get(..8) != Some(INDEX_DELTA_MAGIC_V1.as_slice()) {
        return Err(io::Error::other("invalid pack index delta"));
    }
    let mut at = 8;
    let removed_count = take_index_count(bytes, &mut at, DIGEST_LEN)?;
    let mut removed_packs = Vec::with_capacity(removed_count);
    let mut previous = None;
    for _ in 0..removed_count {
        let pack = PackId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        if previous.is_some_and(|previous| previous >= pack) {
            return Err(io::Error::other("removed packs are not strictly sorted"));
        }
        previous = Some(pack);
        removed_packs.push(pack);
    }
    let removed_manifest_count = take_index_count(bytes, &mut at, DIGEST_LEN)?;
    let mut removed_manifests = HashSet::with_capacity(removed_manifest_count);
    let mut previous = None;
    for _ in 0..removed_manifest_count {
        let manifest = BlobId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        if previous.is_some_and(|previous| previous >= manifest) {
            return Err(io::Error::other(
                "removed manifests are not strictly sorted",
            ));
        }
        previous = Some(manifest);
        removed_manifests.insert(manifest);
    }
    let patch_len = usize::try_from(take_index_u64(bytes, &mut at)?)
        .map_err(|_| io::Error::other("pack index delta length overflow"))?;
    let patch =
        decode_index_checkpoint_without_inventory(take_index_bytes(bytes, &mut at, patch_len)?)?;
    if at != bytes.len() {
        return Err(io::Error::other("trailing bytes in pack index delta"));
    }

    Ok(DecodedIndexDelta {
        removed_packs,
        removed_manifests,
        patch,
    })
}

pub(super) fn apply_decoded_index_delta(index: &mut Index, delta: DecodedIndexDelta) {
    // A run may introduce thousands of packs. Remove their previous locations
    // in one pass, preserving all unaffected duplicate locations, instead of
    // draining and rebuilding the growing overlay once for every pack.
    let affected = delta
        .removed_packs
        .iter()
        .chain(delta.patch.packs.keys())
        .chain(delta.patch.superseded.iter())
        .chain(index.superseded.iter())
        .copied()
        .collect();
    index.chunks.remove_packs(&affected);
    for pack in delta.removed_packs {
        index.packs.remove(&pack);
        index.pack_lengths.remove(&pack);
        index.tombstoned.remove(&pack);
        index.tombstone_records.remove(&pack);
    }
    index.manifests.remove_many(&delta.removed_manifests);
    let Index {
        packs,
        pack_lengths,
        superseded,
        mut tombstoned,
        mut tombstone_records,
        manifests,
        manifests_complete,
        ..
    } = delta.patch;
    for (pack, entries) in packs {
        // A newer exact pack record supersedes an earlier retirement.
        index.superseded.remove(&pack);
        index.packs.remove(&pack);
        index.pack_lengths.remove(&pack);
        index.tombstoned.remove(&pack);
        index.tombstone_records.remove(&pack);
        if let Some(dead) = tombstoned.remove(&pack) {
            index.tombstoned.insert(pack, dead);
        }
        if let Some(records) = tombstone_records.remove(&pack) {
            index.tombstone_records.insert(pack, records);
        }
        let pack_len = pack_lengths
            .get(&pack)
            .copied()
            .expect("decoded pack always has a length");
        if !superseded.contains(&pack) {
            index.add_pack(pack, pack_len, entries);
        }
    }
    index.superseded.extend(superseded);
    index.manifests.merge(manifests);
    index.manifests_complete |= manifests_complete;
    for pack in &index.superseded {
        index.packs.remove(pack);
        index.pack_lengths.remove(pack);
        index.tombstoned.remove(pack);
        index.tombstone_records.remove(pack);
    }
}

#[cfg(test)]
pub(super) fn apply_index_delta(index: &mut Index, bytes: &[u8]) -> io::Result<()> {
    let delta = decode_index_delta(bytes)?;
    apply_decoded_index_delta(index, delta);
    Ok(())
}

fn encode_catalog_base(base: &CatalogBase, bytes: &mut Vec<u8>) -> io::Result<()> {
    match base {
        CatalogBase::Inline(checkpoint) => {
            bytes.push(0);
            bytes.extend_from_slice(&(checkpoint.len() as u64).to_le_bytes());
            bytes.extend_from_slice(checkpoint);
        }
        CatalogBase::Checkpoint(digest) => {
            bytes.push(1);
            bytes.extend_from_slice(digest.as_bytes());
        }
        CatalogBase::Sharded { root, shard_bits } => {
            if *shard_bits == 0 || *shard_bits > MAX_SHARD_BITS {
                return Err(io::Error::other("invalid catalog shard bits"));
            }
            bytes.push(2);
            bytes.extend_from_slice(root.as_bytes());
            bytes.push(*shard_bits);
        }
    }
    Ok(())
}

fn decode_catalog_base(bytes: &[u8], at: &mut usize) -> io::Result<CatalogBase> {
    let tag = take_index_bytes(bytes, at, 1)?[0];
    match tag {
        0 => {
            let len = usize::try_from(take_index_u64(bytes, at)?)
                .map_err(|_| io::Error::other("inline catalog length overflow"))?;
            Ok(CatalogBase::Inline(Bytes::copy_from_slice(
                take_index_bytes(bytes, at, len)?,
            )))
        }
        1 => Ok(CatalogBase::Checkpoint(
            Digest::try_from(take_index_bytes(bytes, at, DIGEST_LEN)?).map_err(io::Error::other)?,
        )),
        2 => {
            let root = Digest::try_from(take_index_bytes(bytes, at, DIGEST_LEN)?)
                .map_err(io::Error::other)?;
            let shard_bits = take_index_bytes(bytes, at, 1)?[0];
            if shard_bits == 0 || shard_bits > MAX_SHARD_BITS {
                return Err(io::Error::other("invalid catalog shard bits"));
            }
            Ok(CatalogBase::Sharded { root, shard_bits })
        }
        _ => Err(io::Error::other("unknown catalog base kind")),
    }
}

fn append_run_refs(bytes: &mut Vec<u8>, runs: &BTreeMap<u8, CatalogRunRef>) {
    bytes.extend_from_slice(&(runs.len() as u64).to_le_bytes());
    for (level, run) in runs {
        bytes.push(*level);
        bytes.extend_from_slice(run.digest.as_bytes());
        bytes.extend_from_slice(&run.first_generation.to_le_bytes());
        bytes.extend_from_slice(&run.last_generation.to_le_bytes());
        bytes.extend_from_slice(&run.encoded_bytes.to_le_bytes());
        match &run.query {
            Some(query) => {
                bytes.push(1);
                bytes.extend_from_slice(&query.offset.to_le_bytes());
                bytes.extend_from_slice(&query.encoded_bytes.to_le_bytes());
                bytes.extend_from_slice(query.digest.as_bytes());
                bytes.extend_from_slice(&(query.routing.len() as u64).to_le_bytes());
                bytes.extend_from_slice(&query.routing);
            }
            None => bytes.push(0),
        }
    }
}

fn validate_run_refs(
    generation: u64,
    runs: &BTreeMap<u8, CatalogRunRef>,
    inline_deltas: usize,
) -> io::Result<()> {
    if runs.len() > 32
        || runs.iter().any(|(level, run)| {
            *level >= 32
                || run.first_generation == 0
                || run.first_generation > run.last_generation
                || run.last_generation > generation
                || run.encoded_bytes == 0
                || run.query.as_ref().is_some_and(|query| {
                    query.encoded_bytes == 0
                        || query.routing.len() > super::run::RUN_QUERY_TAIL_MAX_BYTES
                        || query
                            .offset
                            .checked_add(query.encoded_bytes)
                            .is_none_or(|end| end != run.encoded_bytes)
                })
        })
    {
        return Err(io::Error::other("invalid catalog run reference"));
    }
    let mut by_generation = runs.values().collect::<Vec<_>>();
    by_generation.sort_unstable_by_key(|run| run.first_generation);
    if by_generation
        .windows(2)
        .any(|pair| pair[0].last_generation.checked_add(1) != Some(pair[1].first_generation))
    {
        return Err(io::Error::other(
            "catalog run generations overlap or contain a gap",
        ));
    }
    if let Some(last) = by_generation.last()
        && last.last_generation.saturating_add(inline_deltas as u64) != generation
    {
        return Err(io::Error::other(
            "catalog runs and inline deltas do not reach the root generation",
        ));
    }
    Ok(())
}

fn delta_catalog_checksum(
    generation: u64,
    base: &CatalogBase,
    runs: &BTreeMap<u8, CatalogRunRef>,
    deltas: &[Bytes],
) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"casita pack catalog root v1\0");
    hasher.update(&generation.to_le_bytes());
    let mut encoded_base = Vec::new();
    encode_catalog_base(base, &mut encoded_base).expect("validated catalog base");
    hasher.update(&(encoded_base.len() as u64).to_le_bytes());
    hasher.update(&encoded_base);
    let mut encoded_runs = Vec::new();
    append_run_refs(&mut encoded_runs, runs);
    hasher.update(&encoded_runs);
    for delta in deltas {
        hasher.update(&(delta.len() as u64).to_le_bytes());
        hasher.update(delta);
    }
    Digest::from(hasher.finalize())
}

pub(super) fn encode_delta_catalog(catalog: &DeltaCatalog) -> io::Result<Bytes> {
    if catalog.generation == 0 {
        return Err(io::Error::other("invalid delta catalog generation"));
    }
    if catalog.deltas.len() > MAX_INLINE_DELTAS
        || catalog
            .deltas
            .iter()
            .map(|delta| 8 + delta.len())
            .fold(0_usize, usize::saturating_add)
            > MAX_INLINE_DELTA_BYTES
    {
        return Err(io::Error::other("pack catalog delta chain is too large"));
    }
    validate_run_refs(catalog.generation, &catalog.runs, catalog.deltas.len())?;
    let mut encoded_base = Vec::new();
    encode_catalog_base(&catalog.base, &mut encoded_base)?;
    let checksum = delta_catalog_checksum(
        catalog.generation,
        &catalog.base,
        &catalog.runs,
        &catalog.deltas,
    );
    let checksum = sidecar_checksum(checksum, catalog.sidecars);
    let capacity = 8
        + 8
        + DIGEST_LEN
        + encoded_base.len()
        + 8
        + catalog.runs.len() * (1 + DIGEST_LEN + 8 + 8 + 8 + 1 + 8 + 8 + DIGEST_LEN + 8)
        + catalog
            .runs
            .values()
            .filter_map(|run| run.query.as_ref())
            .map(|query| query.routing.len())
            .sum::<usize>()
        + 8
        + catalog
            .deltas
            .iter()
            .map(|delta| 8 + delta.len())
            .sum::<usize>();
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(if catalog.sidecars.is_some() {
        &INDEX_CATALOG_MAGIC_V2
    } else {
        &INDEX_CATALOG_MAGIC_V1
    });
    bytes.extend_from_slice(&catalog.generation.to_le_bytes());
    bytes.extend_from_slice(checksum.as_bytes());
    bytes.extend_from_slice(&encoded_base);
    append_run_refs(&mut bytes, &catalog.runs);
    bytes.extend_from_slice(&(catalog.deltas.len() as u64).to_le_bytes());
    for delta in &catalog.deltas {
        bytes.extend_from_slice(&(delta.len() as u64).to_le_bytes());
        bytes.extend_from_slice(delta);
    }
    if let Some(root) = catalog.sidecars {
        bytes.extend_from_slice(root.as_bytes());
    }
    Ok(bytes.into())
}

fn sidecar_checksum(checksum: Digest, sidecars: Option<Digest>) -> Digest {
    match sidecars {
        None => checksum,
        Some(root) => {
            let mut hash = blake3::Hasher::new();
            hash.update(b"casita catalog sidecars v2\0");
            hash.update(checksum.as_bytes());
            hash.update(root.as_bytes());
            hash.finalize().into()
        }
    }
}

pub(super) fn delta_catalog_needs_compaction(catalog: &DeltaCatalog, next: &[u8]) -> bool {
    catalog.deltas.len() >= MAX_INLINE_DELTAS
        || catalog
            .deltas
            .iter()
            .map(|delta| 8 + delta.len())
            .fold(0_usize, usize::saturating_add)
            .saturating_add(8 + next.len())
            > MAX_INLINE_DELTA_BYTES
}

pub(super) fn decode_delta_catalog(bytes: &[u8]) -> io::Result<DeltaCatalog> {
    const HEADER_LEN: usize = 8 + 8 + DIGEST_LEN;
    if bytes.len() < HEADER_LEN
        || (bytes[..8] != INDEX_CATALOG_MAGIC_V1 && bytes[..8] != INDEX_CATALOG_MAGIC_V2)
    {
        return Err(io::Error::other("invalid delta catalog"));
    }
    let generation = u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes"));
    if generation == 0 {
        return Err(io::Error::other("invalid delta catalog generation"));
    }
    let expected = Digest::try_from(&bytes[16..HEADER_LEN]).map_err(io::Error::other)?;
    let mut at = HEADER_LEN;
    let base = decode_catalog_base(bytes, &mut at)?;
    let run_count = take_index_count(bytes, &mut at, 1 + DIGEST_LEN + 8 + 8 + 8)?;
    if run_count > 32 {
        return Err(io::Error::other("pack catalog has too many run references"));
    }
    let mut runs = BTreeMap::new();
    for _ in 0..run_count {
        let level = take_index_bytes(bytes, &mut at, 1)?[0];
        let run = CatalogRunRef {
            digest: Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
            first_generation: take_index_u64(bytes, &mut at)?,
            last_generation: take_index_u64(bytes, &mut at)?,
            encoded_bytes: take_index_u64(bytes, &mut at)?,
            query: match take_index_bytes(bytes, &mut at, 1)?[0] {
                0 => None,
                1 => Some(CatalogRunQueryRef {
                    offset: take_index_u64(bytes, &mut at)?,
                    encoded_bytes: take_index_u64(bytes, &mut at)?,
                    digest: Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                        .map_err(io::Error::other)?,
                    routing: {
                        let len = usize::try_from(take_index_u64(bytes, &mut at)?)
                            .map_err(|_| io::Error::other("catalog run routing length overflow"))?;
                        if len > super::run::RUN_QUERY_TAIL_MAX_BYTES {
                            return Err(io::Error::other("catalog run routing is too large"));
                        }
                        Bytes::copy_from_slice(take_index_bytes(bytes, &mut at, len)?)
                    },
                }),
                _ => return Err(io::Error::other("invalid catalog run query flag")),
            },
        };
        if runs.insert(level, run).is_some() {
            return Err(io::Error::other("duplicate catalog run level"));
        }
    }
    let count = take_index_count(bytes, &mut at, 8 + 8)?;
    if count > MAX_INLINE_DELTAS {
        return Err(io::Error::other("pack catalog has too many deltas"));
    }
    let mut deltas = Vec::with_capacity(count);
    let mut encoded_delta_bytes = 0_usize;
    for _ in 0..count {
        let len = usize::try_from(take_index_u64(bytes, &mut at)?)
            .map_err(|_| io::Error::other("delta catalog length overflow"))?;
        encoded_delta_bytes = encoded_delta_bytes.saturating_add(8 + len);
        if encoded_delta_bytes > MAX_INLINE_DELTA_BYTES {
            return Err(io::Error::other("pack catalog delta chain is too large"));
        }
        deltas.push(Bytes::copy_from_slice(take_index_bytes(
            bytes, &mut at, len,
        )?));
    }
    let sidecars = if bytes[..8] == INDEX_CATALOG_MAGIC_V2 {
        Some(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        )
    } else {
        None
    };
    if at != bytes.len() {
        return Err(io::Error::other("trailing bytes in delta catalog"));
    }
    validate_run_refs(generation, &runs, deltas.len())?;
    if sidecar_checksum(
        delta_catalog_checksum(generation, &base, &runs, &deltas),
        sidecars,
    ) != expected
    {
        return Err(io::Error::other("delta catalog checksum mismatch"));
    }
    Ok(DeltaCatalog {
        sidecars,
        generation,
        base,
        runs,
        deltas,
    })
}

#[cfg(test)]
pub(super) fn reopen_delta_catalog(
    catalog: &[u8],
    external_base: Option<&[u8]>,
) -> io::Result<Index> {
    let catalog = decode_delta_catalog(catalog)?;
    let base = match &catalog.base {
        CatalogBase::Inline(base) => base.as_ref(),
        CatalogBase::Checkpoint(expected) => {
            let base = external_base.ok_or_else(|| io::Error::other("catalog base is required"))?;
            if Digest::from(blake3::hash(base)) != *expected {
                return Err(io::Error::other("delta catalog base hash mismatch"));
            }
            base
        }
        CatalogBase::Sharded { .. } => {
            return Err(io::Error::other(
                "sharded catalog requires shard-aware reconstruction",
            ));
        }
    };
    let mut index = decode_index_checkpoint_without_inventory(base)?;
    for delta in &catalog.deltas {
        apply_index_delta(&mut index, delta)?;
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &[u8], offset: u64) -> PackEntry {
        PackEntry {
            digest: ChunkId::new(blake3::hash(label).into()),
            offset,
            framed_len: 32,
            uncompressed_len: 64,
        }
    }

    fn assert_same(left: &Index, right: &Index) {
        let inventory = Digest::from(blake3::hash(b"delta equality inventory"));
        assert_eq!(
            encode_index_checkpoint(left, inventory).unwrap(),
            encode_index_checkpoint(right, inventory).unwrap()
        );
    }

    #[test]
    fn bulk_delta_removal_preserves_duplicates_and_reactivates_exact_packs() {
        let packs =
            [b"one", b"two", b"tri", b"for"].map(|label| PackId::new(blake3::hash(label).into()));
        let shared = entry(b"shared chunk", 0);
        let replaced = entry(b"replacement chunk", 0);
        let mut index = Index::default();
        for pack in &packs[..3] {
            index.add_pack(*pack, 192, vec![shared]);
        }
        index.rebuild_chunks();
        index.superseded.insert(packs[3]);
        let mut patch = Index::default();
        patch.add_pack(packs[1], 192, vec![replaced]);
        patch.add_pack(packs[3], 192, vec![shared]);
        patch.superseded.insert(packs[2]);
        apply_decoded_index_delta(
            &mut index,
            DecodedIndexDelta {
                removed_packs: vec![packs[0]],
                removed_manifests: HashSet::new(),
                patch,
            },
        );
        assert_eq!(index.chunks.get(&shared.digest).unwrap().pack, packs[3]);
        assert_eq!(index.chunks.get(&replaced.digest).unwrap().pack, packs[1]);
        assert!(!index.packs.contains_key(&packs[0]));
        assert!(!index.packs.contains_key(&packs[2]));
        assert!(!index.superseded.contains(&packs[3]));
        assert_eq!(index.chunks.len(), 2);
    }

    #[test]
    fn exact_delta_round_trips_pack_and_manifest_changes() {
        let first = PackId::new(blake3::hash(b"first delta pack").into());
        let second = PackId::new(blake3::hash(b"second delta pack").into());
        let removed = PackId::new(blake3::hash(b"removed delta pack").into());
        let first_entries = vec![entry(b"first chunk", 0), entry(b"dead chunk", 32)];
        let mut base = Index::default();
        base.add_pack(first, 256, first_entries.clone());
        base.add_pack(removed, 192, vec![entry(b"removed chunk", 0)]);
        let removed_manifest = BlobId::new(blake3::hash(b"removed manifest").into());
        base.manifests.insert(removed_manifest);
        base.manifests_complete = true;
        base.rebuild_chunks();

        let mut next = base.clone();
        next.remove_pack(removed);
        next.tombstoned
            .insert(first, HashSet::from([first_entries[1].digest]));
        next.tombstone_records.insert(
            first,
            HashSet::from([Digest::from(blake3::hash(b"tombstone record"))]),
        );
        next.add_pack(second, 192, vec![entry(b"new chunk", 0)]);
        next.manifests
            .insert(BlobId::new(blake3::hash(b"new manifest").into()));
        next.manifests
            .remove_many(&HashSet::from([removed_manifest]));
        next.rebuild_chunks();

        let delta = encode_index_delta(&base, &next).unwrap();
        let mut reconstructed = base;
        apply_index_delta(&mut reconstructed, &delta).unwrap();
        assert_same(&reconstructed, &next);
    }

    #[test]
    fn delta_catalog_reopens_and_compacts_to_an_exact_new_base() {
        let inventory = Digest::from(blake3::hash(b"delta catalog inventory"));
        let pack = PackId::new(blake3::hash(b"catalog pack").into());
        let mut base = Index::default();
        base.add_pack(pack, 192, vec![entry(b"catalog chunk", 0)]);
        base.manifests_complete = true;
        base.rebuild_chunks();
        let base_bytes = encode_index_checkpoint(&base, inventory).unwrap();

        let mut next = base.clone();
        next.manifests
            .insert(BlobId::new(blake3::hash(b"catalog delta manifest").into()));
        let delta = encode_index_delta(&base, &next).unwrap();
        let catalog = DeltaCatalog {
            sidecars: None,
            generation: 1,
            base: CatalogBase::Checkpoint(Digest::from(blake3::hash(&base_bytes))),
            runs: BTreeMap::new(),
            deltas: vec![delta],
        };
        assert!(!delta_catalog_needs_compaction(
            &DeltaCatalog {
                sidecars: None,
                deltas: Vec::new(),
                ..catalog.clone()
            },
            &catalog.deltas[0]
        ));
        let catalog_bytes = encode_delta_catalog(&catalog).unwrap();
        let reopened = reopen_delta_catalog(&catalog_bytes, Some(&base_bytes)).unwrap();
        assert_same(&reopened, &next);

        let compacted_base = encode_index_checkpoint(&reopened, inventory).unwrap();
        let compacted = DeltaCatalog {
            sidecars: None,
            generation: 2,
            base: CatalogBase::Checkpoint(Digest::from(blake3::hash(&compacted_base))),
            runs: BTreeMap::new(),
            deltas: Vec::new(),
        };
        let compacted_catalog = encode_delta_catalog(&compacted).unwrap();
        assert_same(
            &reopen_delta_catalog(&compacted_catalog, Some(&compacted_base)).unwrap(),
            &next,
        );

        let bounded = DeltaCatalog {
            sidecars: None,
            generation: 3,
            base: compacted.base,
            runs: BTreeMap::new(),
            deltas: vec![catalog.deltas[0].clone(); MAX_INLINE_DELTAS],
        };
        assert!(delta_catalog_needs_compaction(&bounded, &catalog.deltas[0]));
    }

    #[test]
    fn delta_catalog_rejects_corruption_and_the_wrong_base() {
        let inventory = Digest::from(blake3::hash(b"corrupt delta inventory"));
        let base = Index {
            manifests_complete: true,
            ..Index::default()
        };
        let base_bytes = encode_index_checkpoint(&base, inventory).unwrap();
        let catalog = encode_delta_catalog(&DeltaCatalog {
            sidecars: None,
            generation: 1,
            base: CatalogBase::Checkpoint(Digest::from(blake3::hash(&base_bytes))),
            runs: BTreeMap::new(),
            deltas: Vec::new(),
        })
        .unwrap();

        let mut corrupt = catalog.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_delta_catalog(&corrupt).is_err());
        let mut unknown_format = catalog.to_vec();
        unknown_format[INDEX_CATALOG_MAGIC_V1.len() - 1] = b'9';
        assert!(decode_delta_catalog(&unknown_format).is_err());
        assert!(reopen_delta_catalog(&catalog, Some(b"wrong base")).is_err());

        let mut trailing = encode_index_delta(&base, &base).unwrap().to_vec();
        trailing.push(0);
        let mut reopened = base;
        assert!(apply_index_delta(&mut reopened, &trailing).is_err());
    }

    #[test]
    fn delta_catalog_rejects_a_gap_between_runs() {
        let run = |first_generation: u64, last_generation: u64| CatalogRunRef {
            digest: Digest::from(blake3::hash(&first_generation.to_le_bytes())),
            first_generation,
            last_generation,
            encoded_bytes: 1,
            query: None,
        };
        let catalog = DeltaCatalog {
            sidecars: None,
            generation: 4,
            base: CatalogBase::Inline(Bytes::new()),
            runs: BTreeMap::from([(0, run(2_u64, 2)), (1, run(4_u64, 4))]),
            deltas: Vec::new(),
        };

        assert!(encode_delta_catalog(&catalog).is_err());
    }

    #[test]
    fn delta_catalog_round_trips_authenticated_run_query_references() {
        let query = CatalogRunQueryRef {
            offset: 1024,
            encoded_bytes: 256,
            digest: Digest::from(blake3::hash(b"catalog run query tail")),
            routing: Bytes::from_static(b"authenticated run routing"),
        };
        let run = CatalogRunRef {
            digest: Digest::from(blake3::hash(b"catalog run with query")),
            first_generation: 1,
            last_generation: 1,
            encoded_bytes: query.offset + query.encoded_bytes,
            query: Some(query),
        };
        let catalog = DeltaCatalog {
            sidecars: None,
            generation: 1,
            base: CatalogBase::Inline(Bytes::new()),
            runs: BTreeMap::from([(0, run)]),
            deltas: Vec::new(),
        };

        let encoded = encode_delta_catalog(&catalog).unwrap();
        assert_eq!(decode_delta_catalog(&encoded).unwrap(), catalog);
    }

    #[test]
    fn exact_mutations_cancel_opposites_and_touch_only_recorded_packs() {
        let first = PackId::new(blake3::hash(b"recorded pack").into());
        let ignored = PackId::new(blake3::hash(b"ignored pack").into());
        let manifest = BlobId::new(blake3::hash(b"cancelled manifest").into());
        let mut next = Index::default();
        next.add_pack(first, 192, vec![entry(b"recorded chunk", 0)]);
        next.add_pack(ignored, 192, vec![entry(b"ignored chunk", 0)]);
        next.rebuild_chunks();

        let mut mutations = IndexMutations::default();
        mutations.record_pack(first);
        mutations.record_manifest_add(manifest);
        mutations.record_manifest_remove(manifest);
        assert!(!mutations.is_empty());

        let mut reconstructed = Index::default();
        apply_index_delta(
            &mut reconstructed,
            &encode_index_mutations(&next, &mutations).unwrap(),
        )
        .unwrap();
        assert!(reconstructed.packs.contains_key(&first));
        assert!(!reconstructed.packs.contains_key(&ignored));
        assert!(!reconstructed.manifests.contains(&manifest));
    }

    #[test]
    fn v1_root_round_trips_inline_checkpoint_and_shard_map() {
        let checkpoint = encode_index_checkpoint(
            &Index::default(),
            Digest::from(blake3::hash(b"inline inventory")),
        )
        .unwrap();
        let inline = DeltaCatalog {
            sidecars: None,
            generation: 7,
            base: CatalogBase::Inline(checkpoint.clone()),
            runs: BTreeMap::new(),
            deltas: Vec::new(),
        };
        let encoded = encode_delta_catalog(&inline).unwrap();
        assert_eq!(decode_delta_catalog(&encoded).unwrap(), inline);
        assert!(reopen_delta_catalog(&encoded, None).is_ok());

        let sharded = DeltaCatalog {
            sidecars: None,
            generation: 8,
            base: CatalogBase::Sharded {
                root: Digest::from(blake3::hash(b"shard map")),
                shard_bits: 12,
            },
            runs: BTreeMap::new(),
            deltas: Vec::new(),
        };
        assert_eq!(
            decode_delta_catalog(&encode_delta_catalog(&sharded).unwrap()).unwrap(),
            sharded
        );
        assert!(
            encode_delta_catalog(&DeltaCatalog {
                sidecars: None,
                generation: 9,
                base: CatalogBase::Sharded {
                    root: Digest::from(blake3::hash(b"bad shard map")),
                    shard_bits: 0,
                },
                runs: BTreeMap::new(),
                deltas: Vec::new(),
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod sidecar_catalog_tests {
    use super::*;
    #[test]
    fn sidecar_reference_is_versioned_authenticated_and_legacy_roots_roundtrip() {
        let legacy = PackedChunks::empty_state_catalog().unwrap();
        let mut root = decode_delta_catalog(&legacy).unwrap();
        assert!(root.sidecars.is_none());
        assert_eq!(encode_delta_catalog(&root).unwrap().as_ref(), legacy);
        root.sidecars = Some(Digest::from([42; 32]));
        let encoded = encode_delta_catalog(&root).unwrap();
        assert!(encoded.starts_with(&INDEX_CATALOG_MAGIC_V2));
        assert_eq!(decode_delta_catalog(&encoded).unwrap(), root);
        let mut corrupt = encoded.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_delta_catalog(&corrupt).is_err());
        let mut downgraded = encoded.to_vec();
        downgraded[..8].copy_from_slice(&INDEX_CATALOG_MAGIC_V1);
        assert!(decode_delta_catalog(&downgraded).is_err());
        assert!(decode_delta_catalog(&encoded[..encoded.len() - 1]).is_err());
    }
}
