//! Immutable, content-addressed catalog delta runs.
//!
//! Runs replace eager rewriting of every digest-prefix shard touched by a
//! publication batch. Each object carries one canonical last-writer-wins delta
//! over a generation interval. Geometric levels can merge these objects while
//! the authoritative pointer retains only a bounded number of references.

use std::collections::{BTreeMap, HashSet};

use super::delta::{
    CatalogRunQueryRef, DecodedIndexDelta, decode_index_delta, encode_decoded_index_delta,
};
use super::*;

const CATALOG_RUN_MAGIC_V1: [u8; 8] = *b"casitarn";
pub(super) const CATALOG_RUN_MAGIC_V2: [u8; 8] = *b"casitrn2";
pub(super) const CATALOG_RUN_DOMAIN: &[u8] = b"casita catalog delta run v1\0";
const RUN_CHUNK_BLOCK_MAGIC_V1: [u8; 8] = *b"casircb1";
const RUN_QUERY_DIRECTORY_MAGIC_V1: [u8; 8] = *b"casirqd1";
pub(super) const RUN_QUERY_TRAILER_MAGIC_V1: [u8; 8] = *b"casirqt1";
pub(super) const RUN_QUERY_TRAILER_BYTES: usize = 8 + 8 + 8 + DIGEST_LEN;
const RUN_QUERY_BLOCK_ENTRIES: usize = 1024;
pub(super) const RUN_QUERY_TAIL_MAX_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_CATALOG_RUN_LEVELS: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CatalogRunChunkBlockRef {
    pub(super) first: ChunkId,
    pub(super) last: ChunkId,
    pub(super) offset: u64,
    pub(super) encoded_bytes: u64,
    pub(super) digest: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogRunQueryIndex {
    pub(super) chunks: Vec<CatalogRunChunkBlockRef>,
    pub(super) changed_packs: Vec<PackId>,
}

impl CatalogRunQueryIndex {
    pub(super) fn chunk_block(&self, digest: &ChunkId) -> Option<CatalogRunChunkBlockRef> {
        let at = self.chunks.partition_point(|block| block.last < *digest);
        self.chunks
            .get(at)
            .copied()
            .filter(|block| block.first <= *digest)
    }

    pub(super) fn changes_pack(&self, pack: &PackId) -> bool {
        self.changed_packs.binary_search(pack).is_ok()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CatalogRun {
    pub(super) first_generation: u64,
    pub(super) last_generation: u64,
    pub(super) delta: Bytes,
}

#[derive(Clone)]
enum PackMutation {
    Removed,
    Superseded,
    Present {
        pack_len: u64,
        entries: Vec<PackEntry>,
        tombstoned: HashSet<ChunkId>,
        tombstone_records: HashSet<Digest>,
    },
}

#[derive(Default)]
struct DeltaAccumulator {
    packs: BTreeMap<PackId, PackMutation>,
    manifests: BTreeMap<BlobId, bool>,
    manifests_complete: bool,
}

impl DeltaAccumulator {
    fn apply(&mut self, delta: DecodedIndexDelta) -> io::Result<()> {
        for pack in delta.removed_packs {
            self.packs.insert(pack, PackMutation::Removed);
        }
        for manifest in delta.removed_manifests {
            self.manifests.insert(manifest, false);
        }
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
            let pack_len = pack_lengths
                .get(&pack)
                .copied()
                .ok_or_else(|| io::Error::other("catalog run pack has no length"))?;
            self.packs.insert(
                pack,
                PackMutation::Present {
                    pack_len,
                    entries,
                    tombstoned: tombstoned.remove(&pack).unwrap_or_default(),
                    tombstone_records: tombstone_records.remove(&pack).unwrap_or_default(),
                },
            );
        }
        for pack in superseded {
            self.packs.insert(pack, PackMutation::Superseded);
        }
        for manifest in manifests.sorted_ids() {
            self.manifests.insert(manifest, true);
        }
        self.manifests_complete |= manifests_complete;
        Ok(())
    }

    fn finish(self) -> DecodedIndexDelta {
        let mut removed_packs = Vec::new();
        let mut patch = Index {
            manifests_complete: self.manifests_complete,
            ..Index::default()
        };
        for (pack, mutation) in self.packs {
            match mutation {
                PackMutation::Removed => removed_packs.push(pack),
                PackMutation::Superseded => {
                    patch.superseded.insert(pack);
                }
                PackMutation::Present {
                    pack_len,
                    entries,
                    tombstoned,
                    tombstone_records,
                } => {
                    patch.add_pack_metadata(pack, pack_len, entries);
                    if !tombstoned.is_empty() {
                        patch.tombstoned.insert(pack, tombstoned);
                    }
                    if !tombstone_records.is_empty() {
                        patch.tombstone_records.insert(pack, tombstone_records);
                    }
                }
            }
        }
        let mut removed_manifests = HashSet::new();
        for (manifest, present) in self.manifests {
            if present {
                patch.manifests.insert(manifest);
            } else {
                removed_manifests.insert(manifest);
            }
        }
        DecodedIndexDelta {
            removed_packs,
            removed_manifests,
            patch,
        }
    }
}

pub(super) fn run_checksum(first_generation: u64, last_generation: u64, delta: &[u8]) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CATALOG_RUN_DOMAIN);
    hasher.update(&first_generation.to_le_bytes());
    hasher.update(&last_generation.to_le_bytes());
    hasher.update(&(delta.len() as u64).to_le_bytes());
    hasher.update(delta);
    Digest::from(hasher.finalize())
}

pub(super) fn encode_catalog_run(run: &CatalogRun) -> io::Result<Bytes> {
    if run.first_generation == 0 || run.first_generation > run.last_generation {
        return Err(io::Error::other("invalid catalog run generations"));
    }
    let decoded = decode_index_delta(&run.delta)?;
    let checksum = run_checksum(run.first_generation, run.last_generation, &run.delta);
    let mut bytes = Vec::with_capacity(8 + 8 + 8 + DIGEST_LEN + 8 + run.delta.len());
    bytes.extend_from_slice(&CATALOG_RUN_MAGIC_V2);
    bytes.extend_from_slice(&run.first_generation.to_le_bytes());
    bytes.extend_from_slice(&run.last_generation.to_le_bytes());
    bytes.extend_from_slice(checksum.as_bytes());
    bytes.extend_from_slice(&(run.delta.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&run.delta);

    // Keep every live location, rather than only the first location returned
    // by `ChunkIndex::get`. Concurrent writers may legitimately seal the same
    // chunk into different packs. Point lookup needs only one such location,
    // but a later streaming rebase needs the complete secondary index so that
    // removing either pack cannot make the other location disappear.
    let mut locations = decoded.patch.chunks.base.clone();
    locations.sort_unstable_by(|left, right| {
        left.digest
            .cmp(&right.digest)
            .then_with(|| left.location.pack.cmp(&right.location.pack))
    });
    let mut blocks = Vec::new();
    let mut start = 0;
    while start < locations.len() {
        let mut end = (start + RUN_QUERY_BLOCK_ENTRIES).min(locations.len());
        // A digest must route to exactly one block. Allow an unusually large
        // duplicate set to make one block larger instead of splitting equal
        // digests across adjacent ranges.
        while end < locations.len() && locations[end - 1].digest == locations[end].digest {
            end += 1;
        }
        let entries = &locations[start..end];
        let offset = bytes.len() as u64;
        let block = encode_run_chunk_block(entries);
        let reference = CatalogRunChunkBlockRef {
            first: entries.first().expect("nonempty chunk block").digest,
            last: entries.last().expect("nonempty chunk block").digest,
            offset,
            encoded_bytes: block.len() as u64,
            digest: Digest::from(blake3::hash(&block)),
        };
        bytes.extend_from_slice(&block);
        blocks.push(reference);
        start = end;
    }
    let mut changed_packs = decoded.removed_packs;
    changed_packs.extend(decoded.patch.packs.keys().copied());
    changed_packs.extend(decoded.patch.superseded.iter().copied());
    changed_packs.sort_unstable();
    changed_packs.dedup();

    let directory_offset = bytes.len() as u64;
    let directory = encode_run_query_directory(&CatalogRunQueryIndex {
        chunks: blocks,
        changed_packs,
    });
    if directory.len() + RUN_QUERY_TRAILER_BYTES > RUN_QUERY_TAIL_MAX_BYTES {
        return Err(io::Error::other("catalog run query directory is too large"));
    }
    let directory_digest = Digest::from(blake3::hash(&directory));
    bytes.extend_from_slice(&directory);
    bytes.extend_from_slice(&RUN_QUERY_TRAILER_MAGIC_V1);
    bytes.extend_from_slice(&directory_offset.to_le_bytes());
    bytes.extend_from_slice(&(directory.len() as u64).to_le_bytes());
    bytes.extend_from_slice(directory_digest.as_bytes());
    Ok(bytes.into())
}

pub(super) fn encode_run_chunk_block(entries: &[IndexedLocation]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + entries.len() * (DIGEST_LEN * 2 + 32));
    bytes.extend_from_slice(&RUN_CHUNK_BLOCK_MAGIC_V1);
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries {
        bytes.extend_from_slice(entry.digest.as_digest().as_bytes());
        bytes.extend_from_slice(entry.location.pack.as_digest().as_bytes());
        bytes.extend_from_slice(&entry.location.pack_len.to_le_bytes());
        bytes.extend_from_slice(&entry.location.offset.to_le_bytes());
        bytes.extend_from_slice(&entry.location.framed_len.to_le_bytes());
        bytes.extend_from_slice(&entry.location.uncompressed_len.to_le_bytes());
    }
    bytes
}

pub(super) fn encode_run_query_directory(index: &CatalogRunQueryIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&RUN_QUERY_DIRECTORY_MAGIC_V1);
    bytes.extend_from_slice(&(index.chunks.len() as u64).to_le_bytes());
    for block in &index.chunks {
        bytes.extend_from_slice(block.first.as_digest().as_bytes());
        bytes.extend_from_slice(block.last.as_digest().as_bytes());
        bytes.extend_from_slice(&block.offset.to_le_bytes());
        bytes.extend_from_slice(&block.encoded_bytes.to_le_bytes());
        bytes.extend_from_slice(block.digest.as_bytes());
    }
    bytes.extend_from_slice(&(index.changed_packs.len() as u64).to_le_bytes());
    for pack in &index.changed_packs {
        bytes.extend_from_slice(pack.as_digest().as_bytes());
    }
    bytes
}

pub(super) fn encode_run_query_tail(
    index: &CatalogRunQueryIndex,
    directory_offset: u64,
) -> io::Result<Bytes> {
    let directory = encode_run_query_directory(index);
    if directory.len() + RUN_QUERY_TRAILER_BYTES > RUN_QUERY_TAIL_MAX_BYTES {
        return Err(io::Error::other("catalog run query directory is too large"));
    }
    let digest = Digest::from(blake3::hash(&directory));
    let mut bytes = Vec::with_capacity(directory.len() + RUN_QUERY_TRAILER_BYTES);
    bytes.extend_from_slice(&directory);
    bytes.extend_from_slice(&RUN_QUERY_TRAILER_MAGIC_V1);
    bytes.extend_from_slice(&directory_offset.to_le_bytes());
    bytes.extend_from_slice(&(directory.len() as u64).to_le_bytes());
    bytes.extend_from_slice(digest.as_bytes());
    Ok(bytes.into())
}

pub(super) fn catalog_run_query_tail(encoded_bytes: u64) -> io::Result<Range<usize>> {
    let end = usize::try_from(encoded_bytes)
        .map_err(|_| io::Error::other("catalog run length overflows address space"))?;
    Ok(end.saturating_sub(RUN_QUERY_TAIL_MAX_BYTES)..end)
}

pub(super) fn catalog_run_query_ref(bytes: &[u8]) -> io::Result<CatalogRunQueryRef> {
    if bytes.len() < RUN_QUERY_TRAILER_BYTES {
        return Err(io::Error::other("catalog run has no query trailer"));
    }
    let trailer = bytes.len() - RUN_QUERY_TRAILER_BYTES;
    if bytes[trailer..trailer + 8] != RUN_QUERY_TRAILER_MAGIC_V1 {
        return Err(io::Error::other("catalog run has no query index"));
    }
    let offset = u64::from_le_bytes(
        bytes[trailer + 8..trailer + 16]
            .try_into()
            .expect("eight bytes"),
    );
    let start = usize::try_from(offset)
        .map_err(|_| io::Error::other("catalog run query offset overflow"))?;
    if start >= bytes.len() {
        return Err(io::Error::other(
            "catalog run query offset is outside object",
        ));
    }
    decode_catalog_run_query_tail(&bytes[start..], start, bytes.len())?;
    Ok(CatalogRunQueryRef {
        offset,
        encoded_bytes: (bytes.len() - start) as u64,
        digest: Digest::from(blake3::hash(&bytes[start..])),
        routing: Bytes::copy_from_slice(&bytes[start..trailer]),
    })
}

pub(super) fn decode_catalog_run_routing(
    reference: &CatalogRunQueryRef,
) -> io::Result<CatalogRunQueryIndex> {
    let directory_offset = usize::try_from(reference.offset)
        .map_err(|_| io::Error::other("catalog run query offset overflow"))?;
    decode_run_query_directory(&reference.routing, directory_offset)
}

pub(super) fn decode_catalog_run_query_tail(
    bytes: &[u8],
    fetched_start: usize,
    object_len: usize,
) -> io::Result<CatalogRunQueryIndex> {
    if bytes.len() < RUN_QUERY_TRAILER_BYTES || fetched_start + bytes.len() != object_len {
        return Err(io::Error::other("invalid catalog run query tail"));
    }
    let trailer = bytes.len() - RUN_QUERY_TRAILER_BYTES;
    if bytes[trailer..trailer + 8] != RUN_QUERY_TRAILER_MAGIC_V1 {
        return Err(io::Error::other("catalog run has no query index"));
    }
    let directory_offset = usize::try_from(u64::from_le_bytes(
        bytes[trailer + 8..trailer + 16]
            .try_into()
            .expect("eight bytes"),
    ))
    .map_err(|_| io::Error::other("catalog run query offset overflow"))?;
    let directory_len = usize::try_from(u64::from_le_bytes(
        bytes[trailer + 16..trailer + 24]
            .try_into()
            .expect("eight bytes"),
    ))
    .map_err(|_| io::Error::other("catalog run query length overflow"))?;
    if directory_offset < fetched_start
        || directory_offset
            .checked_add(directory_len)
            .is_none_or(|end| end != object_len - RUN_QUERY_TRAILER_BYTES)
    {
        return Err(io::Error::other(
            "catalog run query directory lies outside tail",
        ));
    }
    let relative = directory_offset - fetched_start;
    let directory = &bytes[relative..relative + directory_len];
    let expected = Digest::try_from(&bytes[trailer + 24..]).map_err(io::Error::other)?;
    if Digest::from(blake3::hash(directory)) != expected {
        return Err(io::Error::other(
            "catalog run query directory checksum mismatch",
        ));
    }
    decode_run_query_directory(directory, directory_offset)
}

fn decode_run_query_directory(
    bytes: &[u8],
    directory_offset: usize,
) -> io::Result<CatalogRunQueryIndex> {
    if bytes.len() < 16 || bytes[..8] != RUN_QUERY_DIRECTORY_MAGIC_V1 {
        return Err(io::Error::other("invalid catalog run query directory"));
    }
    let mut at = 8;
    let count = take_index_count(bytes, &mut at, DIGEST_LEN * 3 + 16)?;
    let mut chunks = Vec::with_capacity(count);
    for _ in 0..count {
        let first = ChunkId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        let last = ChunkId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        let offset = take_index_u64(bytes, &mut at)?;
        let encoded_bytes = take_index_u64(bytes, &mut at)?;
        let digest = Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?;
        if first > last
            || offset
                .checked_add(encoded_bytes)
                .is_none_or(|end| end > directory_offset as u64)
            || chunks
                .last()
                .is_some_and(|previous: &CatalogRunChunkBlockRef| previous.last >= first)
        {
            return Err(io::Error::other(
                "invalid catalog run chunk block reference",
            ));
        }
        chunks.push(CatalogRunChunkBlockRef {
            first,
            last,
            offset,
            encoded_bytes,
            digest,
        });
    }
    let pack_count = take_index_count(bytes, &mut at, DIGEST_LEN)?;
    let mut changed_packs = Vec::with_capacity(pack_count);
    for _ in 0..pack_count {
        let pack = PackId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        if changed_packs
            .last()
            .is_some_and(|previous| *previous >= pack)
        {
            return Err(io::Error::other("unsorted catalog run changed-pack index"));
        }
        changed_packs.push(pack);
    }
    if at != bytes.len() {
        return Err(io::Error::other(
            "trailing catalog run query directory bytes",
        ));
    }
    Ok(CatalogRunQueryIndex {
        chunks,
        changed_packs,
    })
}

pub(super) fn lookup_catalog_run_chunk_block(
    bytes: &[u8],
    reference: CatalogRunChunkBlockRef,
    digest: &ChunkId,
) -> io::Result<Option<Location>> {
    if bytes.len() as u64 != reference.encoded_bytes
        || Digest::from(blake3::hash(bytes)) != reference.digest
        || bytes.len() < 16
        || bytes[..8] != RUN_CHUNK_BLOCK_MAGIC_V1
    {
        return Err(io::Error::other(
            "catalog run chunk block identity mismatch",
        ));
    }
    let mut at = 8;
    let count = take_index_count(bytes, &mut at, DIGEST_LEN * 2 + 32)?;
    let record_bytes = DIGEST_LEN * 2 + 32;
    if at + count * record_bytes != bytes.len() {
        return Err(io::Error::other("invalid catalog run chunk block length"));
    }
    let body = &bytes[at..];
    let wanted = digest.as_digest().as_bytes().as_slice();
    let mut left = 0;
    let mut right = count;
    while left < right {
        let middle = left + (right - left) / 2;
        let record = &body[middle * record_bytes..(middle + 1) * record_bytes];
        if &record[..DIGEST_LEN] < wanted {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    if left == count || &body[left * record_bytes..left * record_bytes + DIGEST_LEN] != wanted {
        return Ok(None);
    }
    let record = &body[left * record_bytes..(left + 1) * record_bytes];
    let pack = PackId::new(
        Digest::try_from(&record[DIGEST_LEN..DIGEST_LEN * 2]).map_err(io::Error::other)?,
    );
    let mut numbers = DIGEST_LEN * 2;
    let mut take = || {
        let value = u64::from_le_bytes(
            record[numbers..numbers + 8]
                .try_into()
                .expect("eight bytes"),
        );
        numbers += 8;
        value
    };
    Ok(Some(Location {
        pack,
        pack_len: take(),
        offset: take(),
        framed_len: take(),
        uncompressed_len: take(),
    }))
}

pub(super) fn decode_catalog_run(bytes: &[u8]) -> io::Result<CatalogRun> {
    const HEADER_LEN: usize = 8 + 8 + 8 + DIGEST_LEN + 8;
    if bytes.len() < HEADER_LEN
        || (bytes[..8] != CATALOG_RUN_MAGIC_V1 && bytes[..8] != CATALOG_RUN_MAGIC_V2)
    {
        return Err(io::Error::other("invalid catalog delta run"));
    }
    let first_generation = u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes"));
    let last_generation = u64::from_le_bytes(bytes[16..24].try_into().expect("eight bytes"));
    if first_generation == 0 || first_generation > last_generation {
        return Err(io::Error::other("invalid catalog run generations"));
    }
    let expected = Digest::try_from(&bytes[24..24 + DIGEST_LEN]).map_err(io::Error::other)?;
    let mut at = 24 + DIGEST_LEN;
    let delta_len = usize::try_from(take_index_u64(bytes, &mut at)?)
        .map_err(|_| io::Error::other("catalog run length overflow"))?;
    let delta = Bytes::copy_from_slice(take_index_bytes(bytes, &mut at, delta_len)?);
    if run_checksum(first_generation, last_generation, &delta) != expected {
        return Err(io::Error::other("catalog delta run checksum mismatch"));
    }
    if bytes[..8] == CATALOG_RUN_MAGIC_V1 {
        if at != bytes.len() {
            return Err(io::Error::other("trailing catalog delta run bytes"));
        }
    } else {
        let tail = catalog_run_query_tail(bytes.len() as u64)?;
        decode_catalog_run_query_tail(&bytes[tail.clone()], tail.start, bytes.len())?;
    }
    decode_index_delta(&delta)?;
    Ok(CatalogRun {
        first_generation,
        last_generation,
        delta,
    })
}

pub(super) fn merge_catalog_runs(runs: &[CatalogRun]) -> io::Result<CatalogRun> {
    let first = runs
        .first()
        .ok_or_else(|| io::Error::other("cannot merge an empty catalog run set"))?;
    let mut previous_generation = 0_u64;
    let mut accumulator = DeltaAccumulator::default();
    for run in runs {
        if (previous_generation != 0
            && previous_generation.checked_add(1) != Some(run.first_generation))
            || run.first_generation > run.last_generation
        {
            return Err(io::Error::other(
                "catalog runs overlap, contain a gap, or are not generation ordered",
            ));
        }
        accumulator.apply(decode_index_delta(&run.delta)?)?;
        previous_generation = run.last_generation;
    }
    let merged = accumulator.finish();
    Ok(CatalogRun {
        first_generation: first.first_generation,
        last_generation: previous_generation,
        delta: encode_decoded_index_delta(&merged)?,
    })
}

/// Insert the newest sealed batch into a binary leveled run set.
///
/// Each occupied level is one immutable object that must be fetched for a
/// carry. The caller uploads only the final merged object and then CAS-updates
/// the root, so a publication uses `carry_reads + 2` object requests instead
/// of fetching and rewriting every affected digest-prefix shard.
#[cfg(test)]
fn place_catalog_run(
    levels: &mut BTreeMap<u8, CatalogRun>,
    mut incoming: CatalogRun,
) -> io::Result<u8> {
    decode_index_delta(&incoming.delta)?;
    let mut level = 0_u8;
    let mut carry_reads = 0_u8;
    loop {
        if level >= MAX_CATALOG_RUN_LEVELS {
            return Err(io::Error::other("catalog run levels are exhausted"));
        }
        let Some(existing) = levels.remove(&level) else {
            levels.insert(level, incoming);
            return Ok(carry_reads);
        };
        let mut pair = [existing, incoming];
        pair.sort_unstable_by_key(|run| run.first_generation);
        incoming = merge_catalog_runs(&pair)?;
        carry_reads += 1;
        level += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::super::delta::{apply_index_delta, encode_index_delta};
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
        let inventory = Digest::from(blake3::hash(b"catalog run equality inventory"));
        assert_eq!(
            encode_index_checkpoint(left, inventory).unwrap(),
            encode_index_checkpoint(right, inventory).unwrap()
        );
    }

    #[test]
    fn catalog_run_round_trips_and_rejects_corruption() {
        let delta = encode_index_delta(&Index::default(), &Index::default()).unwrap();
        let run = CatalogRun {
            first_generation: 2,
            last_generation: 7,
            delta,
        };
        let encoded = encode_catalog_run(&run).unwrap();
        assert!(decode_catalog_run(&encoded).is_ok_and(|decoded| decoded == run));
        let mut corrupt = encoded.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_catalog_run(&corrupt).is_err());
    }

    #[test]
    fn catalog_run_query_tail_finds_one_authenticated_chunk_block() {
        let pack = PackId::new(blake3::hash(b"queryable catalog run pack").into());
        let wanted = entry(b"queryable catalog run chunk", 64);
        let mut next = Index::default();
        next.add_pack(pack, 4096, vec![wanted]);
        next.rebuild_chunks();
        let run = CatalogRun {
            first_generation: 1,
            last_generation: 1,
            delta: encode_index_delta(&Index::default(), &next).unwrap(),
        };
        let encoded = encode_catalog_run(&run).unwrap();
        let query = catalog_run_query_ref(&encoded).unwrap();
        let start = usize::try_from(query.offset).unwrap();
        let index = decode_catalog_run_query_tail(&encoded[start..], start, encoded.len()).unwrap();
        let reference = index.chunk_block(&wanted.digest).unwrap();
        let block_start = usize::try_from(reference.offset).unwrap();
        let block_end = block_start + usize::try_from(reference.encoded_bytes).unwrap();
        let found = lookup_catalog_run_chunk_block(
            &encoded[block_start..block_end],
            reference,
            &wanted.digest,
        )
        .unwrap()
        .unwrap();

        assert_eq!(found.pack, pack);
        assert_eq!(found.pack_len, 4096);
        assert_eq!(found.offset, wanted.offset);
        assert_eq!(found.framed_len, wanted.framed_len);
        assert_eq!(found.uncompressed_len, wanted.uncompressed_len);
        assert!(index.changes_pack(&pack));
    }

    #[test]
    fn catalog_run_query_keeps_duplicate_live_locations_together() {
        let first_pack = PackId::new(blake3::hash(b"duplicate run pack one").into());
        let second_pack = PackId::new(blake3::hash(b"duplicate run pack two").into());
        let duplicate = entry(b"duplicate run chunk", 0);
        let mut next = Index::default();
        next.add_pack(first_pack, 128, vec![duplicate]);
        next.add_pack(second_pack, 256, vec![duplicate]);
        next.rebuild_chunks();
        let encoded = encode_catalog_run(&CatalogRun {
            first_generation: 1,
            last_generation: 1,
            delta: encode_index_delta(&Index::default(), &next).unwrap(),
        })
        .unwrap();
        let query = catalog_run_query_ref(&encoded).unwrap();
        let index = decode_catalog_run_routing(&query).unwrap();
        let block = index.chunk_block(&duplicate.digest).unwrap();
        let start = usize::try_from(block.offset).unwrap();
        let end = start + usize::try_from(block.encoded_bytes).unwrap();
        let encoded_block = &encoded[start..end];
        let count = u64::from_le_bytes(encoded_block[8..16].try_into().unwrap());

        assert_eq!(count, 2);
        assert_eq!(index.chunks.len(), 1);
        assert!(index.changes_pack(&first_pack));
        assert!(index.changes_pack(&second_pack));
    }

    #[test]
    fn merged_runs_preserve_exact_last_writer_wins_state() {
        let retained_pack = PackId::new(blake3::hash(b"retained run pack").into());
        let removed_pack = PackId::new(blake3::hash(b"removed run pack").into());
        let added_pack = PackId::new(blake3::hash(b"added run pack").into());
        let restored_manifest = BlobId::new(blake3::hash(b"restored run manifest").into());
        let removed_manifest = BlobId::new(blake3::hash(b"removed run manifest").into());

        let retained_entries = vec![
            entry(b"retained live run chunk", 0),
            entry(b"dead run chunk", 32),
        ];
        let mut base = Index::default();
        base.add_pack(retained_pack, 256, retained_entries.clone());
        base.add_pack(removed_pack, 128, vec![entry(b"removed run chunk", 0)]);
        base.manifests.extend([restored_manifest, removed_manifest]);
        base.manifests_complete = true;
        base.rebuild_chunks();

        let mut middle = base.clone();
        middle.remove_pack(removed_pack);
        middle
            .tombstoned
            .insert(retained_pack, HashSet::from([retained_entries[1].digest]));
        middle.manifests.remove(&restored_manifest);
        middle.manifests.remove(&removed_manifest);
        middle.rebuild_chunks();

        let mut final_index = middle.clone();
        final_index.add_pack(added_pack, 128, vec![entry(b"added run chunk", 0)]);
        final_index.manifests.insert(restored_manifest);
        final_index.rebuild_chunks();

        let first = CatalogRun {
            first_generation: 2,
            last_generation: 2,
            delta: encode_index_delta(&base, &middle).unwrap(),
        };
        let second = CatalogRun {
            first_generation: 3,
            last_generation: 3,
            delta: encode_index_delta(&middle, &final_index).unwrap(),
        };
        let merged = merge_catalog_runs(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(merged.first_generation, 2);
        assert_eq!(merged.last_generation, 3);
        let mut reconstructed = base;
        apply_index_delta(&mut reconstructed, &merged.delta).unwrap();
        assert_same(&reconstructed, &final_index);

        assert!(merge_catalog_runs(&[second, first]).is_err());
    }

    #[test]
    fn binary_levels_bound_root_refs_and_request_amplification() {
        const BATCHES: u64 = 1024;
        let mut levels = BTreeMap::new();
        let mut carry_reads = 0_u64;
        for generation in 1..=BATCHES {
            let manifest = BlobId::new(blake3::hash(&generation.to_le_bytes()).into());
            let mut patch = Index::default();
            patch.manifests.insert(manifest);
            let delta = encode_decoded_index_delta(&DecodedIndexDelta {
                removed_packs: Vec::new(),
                removed_manifests: HashSet::new(),
                patch,
            })
            .unwrap();
            carry_reads += u64::from(
                place_catalog_run(
                    &mut levels,
                    CatalogRun {
                        first_generation: generation,
                        last_generation: generation,
                        delta,
                    },
                )
                .unwrap(),
            );
        }
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[&10].first_generation, 1);
        assert_eq!(levels[&10].last_generation, BATCHES);
        assert_eq!(carry_reads, BATCHES - 1);
        let publication_requests = BATCHES * 2 + carry_reads;
        assert_eq!(publication_requests, 3071);
        assert!(publication_requests < BATCHES * 4);

        let mut reconstructed = Index::default();
        let run = levels.remove(&10).unwrap();
        apply_index_delta(&mut reconstructed, &run.delta).unwrap();
        assert_eq!(reconstructed.manifests.len(), BATCHES as usize);
    }
}
