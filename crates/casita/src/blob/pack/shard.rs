//! Immutable digest-prefix catalog shards.
//!
//! This module defines the objects referenced by the v1 `Sharded` base. The
//! three tables deliberately serve different access orders: chunks provide
//! digest-to-pack point lookup, manifests provide exact membership, and pack
//! state supports collection without scanning every chunk shard.

use std::collections::{BTreeMap, HashSet};

use super::*;

const SHARD_MAP_MAGIC_V2: [u8; 8] = *b"casitas2";
const SHARD_MAP_MAGIC_V3: [u8; 8] = *b"casitas3";
const CHUNK_SHARD_MAGIC_V2: [u8; 8] = *b"casitacy";
const CHUNK_BLOCK_ENTRIES: usize = 1024;
const CHUNK_HEADER_LEN: usize = 21;
const CHUNK_ROUTE_LEN: usize = 32 * 3 + 8 * 2;
const CHUNK_SHARD_MAGIC_V1: [u8; 8] = *b"casitacx";
const MANIFEST_SHARD_MAGIC_V1: [u8; 8] = *b"casitamx";
const PACK_SHARD_MAGIC_V1: [u8; 8] = *b"casitapk";
const SHARD_INVENTORY: &[u8] = b"casita pack-state shard v1\0";
const MAX_SHARD_BITS: u8 = 24;
const SHARD_REF_LEN: usize = 4 + DIGEST_LEN + 8 + 8;
const CHUNK_LOCATION_LEN: usize = DIGEST_LEN * 2 + 8 * 4;
const MAX_RUN_ROUTING_BYTES: usize = 32 * 1024 * 1024;
pub(super) const DEFAULT_SHARD_TARGET_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ShardRef {
    pub(super) prefix: u32,
    pub(super) digest: Digest,
    pub(super) entries: u64,
    pub(super) encoded_bytes: u64,
    /// Authenticated routing footer in the same immutable object (v3 maps).
    pub(super) routing: Option<(Digest, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShardMap {
    pub(super) shard_bits: u8,
    pub(super) chunks: Vec<ShardRef>,
    pub(super) manifests: Vec<ShardRef>,
    pub(super) packs: Vec<ShardRef>,
    /// Exact run block routing fetched as part of the ordinary shard-map GET.
    /// Keys are immutable run object digests.
    pub(super) run_routing: BTreeMap<Digest, Bytes>,
}

pub(super) struct EncodedShards {
    pub(super) map: Bytes,
    pub(super) map_digest: Digest,
    pub(super) objects: Vec<(Digest, Bytes)>,
}

fn validate_shard_bits(shard_bits: u8) -> io::Result<()> {
    if shard_bits == 0 || shard_bits > MAX_SHARD_BITS {
        return Err(io::Error::other("invalid catalog shard bits"));
    }
    Ok(())
}

pub(super) fn recommended_shard_bits(entries: u64, target_bytes: u64) -> io::Result<u8> {
    if target_bytes == 0 {
        return Err(io::Error::other("catalog shard target must be non-zero"));
    }
    let total_bytes = u128::from(entries).saturating_mul(CHUNK_LOCATION_LEN as u128);
    let shards = total_bytes.div_ceil(u128::from(target_bytes)).max(1);
    let mut bits = 1_u8;
    while (1_u128 << bits) < shards && bits < MAX_SHARD_BITS {
        bits += 1;
    }
    if (1_u128 << bits) < shards {
        return Err(io::Error::other("catalog exceeds maximum shard width"));
    }
    Ok(bits)
}

pub(super) fn digest_prefix(digest: &Digest, shard_bits: u8) -> io::Result<u32> {
    validate_shard_bits(shard_bits)?;
    let bytes = digest.as_bytes();
    let leading = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], 0]);
    Ok(leading >> (32 - u32::from(shard_bits)))
}

fn append_refs(bytes: &mut Vec<u8>, refs: &[ShardRef]) {
    bytes.extend_from_slice(&(refs.len() as u64).to_le_bytes());
    for shard in refs {
        bytes.extend_from_slice(&shard.prefix.to_le_bytes());
        bytes.extend_from_slice(shard.digest.as_bytes());
        bytes.extend_from_slice(&shard.entries.to_le_bytes());
        bytes.extend_from_slice(&shard.encoded_bytes.to_le_bytes());
        let (digest, length) = shard.routing.unwrap_or((Digest::from([0u8; 32]), 0));
        bytes.extend_from_slice(digest.as_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
    }
}

fn validate_refs(refs: &[ShardRef], shard_bits: u8) -> io::Result<()> {
    let limit = 1_u32 << shard_bits;
    let mut previous = None;
    for shard in refs {
        if shard.prefix >= limit
            || shard.entries == 0
            || shard.encoded_bytes == 0
            || shard
                .routing
                .is_some_and(|(_, len)| len < 8 || len >= shard.encoded_bytes)
            || previous.is_some_and(|previous| previous >= shard.prefix)
        {
            return Err(io::Error::other("invalid catalog shard reference"));
        }
        if let Some((_, length)) = shard.routing {
            let expected_footer = shard
                .entries
                .div_ceil(CHUNK_BLOCK_ENTRIES as u64)
                .checked_mul(CHUNK_ROUTE_LEN as u64)
                .and_then(|n| n.checked_add(8));
            let expected_total = shard
                .entries
                .checked_mul(CHUNK_LOCATION_LEN as u64)
                .and_then(|n| n.checked_add(CHUNK_HEADER_LEN as u64))
                .and_then(|n| n.checked_add(length));
            if expected_footer != Some(length) || expected_total != Some(shard.encoded_bytes) {
                return Err(io::Error::other("invalid routed chunk shard size"));
            }
        }
        previous = Some(shard.prefix);
    }
    Ok(())
}

pub(super) fn encode_shard_map(map: &ShardMap) -> io::Result<Bytes> {
    validate_shard_bits(map.shard_bits)?;
    validate_refs(&map.chunks, map.shard_bits)?;
    validate_refs(&map.manifests, map.shard_bits)?;
    validate_refs(&map.packs, map.shard_bits)?;
    let routing_bytes = map.run_routing.values().map(Bytes::len).sum::<usize>();
    if map.run_routing.len() > super::run::MAX_CATALOG_RUN_LEVELS as usize
        || routing_bytes > MAX_RUN_ROUTING_BYTES
        || map.run_routing.values().any(Bytes::is_empty)
    {
        return Err(io::Error::other("invalid catalog run routing table"));
    }
    let mut body = Vec::with_capacity(
        1 + 24 + SHARD_REF_LEN * (map.chunks.len() + map.manifests.len() + map.packs.len()),
    );
    body.push(map.shard_bits);
    append_refs(&mut body, &map.chunks);
    append_refs(&mut body, &map.manifests);
    append_refs(&mut body, &map.packs);
    body.extend_from_slice(&(map.run_routing.len() as u64).to_le_bytes());
    for (digest, routing) in &map.run_routing {
        body.extend_from_slice(digest.as_bytes());
        body.extend_from_slice(&(routing.len() as u64).to_le_bytes());
        body.extend_from_slice(routing);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"casita catalog shard map v3\0");
    hasher.update(&body);
    let mut bytes = Vec::with_capacity(8 + DIGEST_LEN + body.len());
    bytes.extend_from_slice(&SHARD_MAP_MAGIC_V3);
    bytes.extend_from_slice(hasher.finalize().as_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes.into())
}

fn take_u32(bytes: &[u8], at: &mut usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        take_index_bytes(bytes, at, 4)?
            .try_into()
            .expect("four bytes"),
    ))
}

fn decode_refs(
    bytes: &[u8],
    at: &mut usize,
    shard_bits: u8,
    v3: bool,
) -> io::Result<Vec<ShardRef>> {
    let count = take_index_count(bytes, at, SHARD_REF_LEN + if v3 { 40 } else { 0 })?;
    let mut refs = Vec::with_capacity(count);
    for _ in 0..count {
        refs.push(ShardRef {
            prefix: take_u32(bytes, at)?,
            digest: Digest::try_from(take_index_bytes(bytes, at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
            entries: take_index_u64(bytes, at)?,
            encoded_bytes: take_index_u64(bytes, at)?,
            routing: if v3 {
                let digest = Digest::try_from(take_index_bytes(bytes, at, DIGEST_LEN)?)
                    .map_err(io::Error::other)?;
                let len = take_index_u64(bytes, at)?;
                (len != 0).then_some((digest, len))
            } else {
                None
            },
        });
    }
    validate_refs(&refs, shard_bits)?;
    Ok(refs)
}

pub(super) fn decode_shard_map(bytes: &[u8]) -> io::Result<ShardMap> {
    const HEADER_LEN: usize = 8 + DIGEST_LEN;
    if bytes.len() < HEADER_LEN + 1
        || !matches!(bytes.get(..8), Some(magic) if magic == SHARD_MAP_MAGIC_V2 || magic == SHARD_MAP_MAGIC_V3)
    {
        return Err(io::Error::other("invalid catalog shard map"));
    }
    let expected = Digest::try_from(&bytes[8..HEADER_LEN]).map_err(io::Error::other)?;
    let mut hasher = blake3::Hasher::new();
    let v3 = bytes[..8] == SHARD_MAP_MAGIC_V3;
    hasher.update(if v3 {
        b"casita catalog shard map v3\0"
    } else {
        b"casita catalog shard map v2\0"
    });
    hasher.update(&bytes[HEADER_LEN..]);
    if Digest::from(hasher.finalize()) != expected {
        return Err(io::Error::other("catalog shard map checksum mismatch"));
    }
    let mut at = HEADER_LEN;
    let shard_bits = take_index_bytes(bytes, &mut at, 1)?[0];
    if shard_bits == 0 || shard_bits > MAX_SHARD_BITS {
        return Err(io::Error::other("invalid catalog shard bits"));
    }
    let chunks = decode_refs(bytes, &mut at, shard_bits, v3)?;
    let manifests = decode_refs(bytes, &mut at, shard_bits, v3)?;
    let packs = decode_refs(bytes, &mut at, shard_bits, v3)?;
    let routing_count = take_index_count(bytes, &mut at, DIGEST_LEN + 8)?;
    if routing_count > super::run::MAX_CATALOG_RUN_LEVELS as usize {
        return Err(io::Error::other("too many catalog run routing entries"));
    }
    let mut run_routing = BTreeMap::new();
    let mut routing_bytes = 0_usize;
    for _ in 0..routing_count {
        let digest = Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?;
        let len = usize::try_from(take_index_u64(bytes, &mut at)?)
            .map_err(|_| io::Error::other("catalog run routing length overflow"))?;
        routing_bytes = routing_bytes.saturating_add(len);
        if len == 0 || routing_bytes > MAX_RUN_ROUTING_BYTES {
            return Err(io::Error::other("invalid catalog run routing length"));
        }
        let routing = Bytes::copy_from_slice(take_index_bytes(bytes, &mut at, len)?);
        if run_routing.insert(digest, routing).is_some() {
            return Err(io::Error::other("duplicate catalog run routing entry"));
        }
    }
    let map = ShardMap {
        shard_bits,
        chunks,
        manifests,
        packs,
        run_routing,
    };
    if at != bytes.len() {
        return Err(io::Error::other("trailing bytes in catalog shard map"));
    }
    Ok(map)
}

fn encode_chunk_shard(
    shard_bits: u8,
    prefix: u32,
    entries: &mut [IndexedLocation],
) -> io::Result<Bytes> {
    entries.sort_unstable_by(|left, right| {
        left.digest
            .cmp(&right.digest)
            .then_with(|| left.location.pack.cmp(&right.location.pack))
    });
    if entries.iter().any(|entry| {
        !digest_prefix(entry.digest.as_digest(), shard_bits).is_ok_and(|known| known == prefix)
    }) {
        return Err(io::Error::other("chunk lies outside its catalog shard"));
    }
    let mut bytes = Vec::with_capacity(8 + 1 + 4 + 8 + entries.len() * CHUNK_LOCATION_LEN);
    bytes.extend_from_slice(&CHUNK_SHARD_MAGIC_V2);
    bytes.push(shard_bits);
    bytes.extend_from_slice(&prefix.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for entry in entries.iter() {
        bytes.extend_from_slice(entry.digest.as_digest().as_bytes());
        bytes.extend_from_slice(entry.location.pack.as_digest().as_bytes());
        bytes.extend_from_slice(&entry.location.pack_len.to_le_bytes());
        bytes.extend_from_slice(&entry.location.offset.to_le_bytes());
        bytes.extend_from_slice(&entry.location.framed_len.to_le_bytes());
        bytes.extend_from_slice(&entry.location.uncompressed_len.to_le_bytes());
    }
    let mut routing = Vec::new();
    routing.extend_from_slice(&(entries.len().div_ceil(CHUNK_BLOCK_ENTRIES) as u64).to_le_bytes());
    for (ordinal, block) in entries.chunks(CHUNK_BLOCK_ENTRIES).enumerate() {
        let offset = CHUNK_HEADER_LEN + ordinal * CHUNK_BLOCK_ENTRIES * CHUNK_LOCATION_LEN;
        let length = block.len() * CHUNK_LOCATION_LEN;
        routing.extend_from_slice(block.first().unwrap().digest.as_digest().as_bytes());
        routing.extend_from_slice(block.last().unwrap().digest.as_digest().as_bytes());
        routing.extend_from_slice(&(offset as u64).to_le_bytes());
        routing.extend_from_slice(&(length as u64).to_le_bytes());
        routing.extend_from_slice(blake3::hash(&bytes[offset..offset + length]).as_bytes());
    }
    bytes.extend_from_slice(&routing);
    Ok(bytes.into())
}

fn decode_chunk_header(bytes: &[u8], expected_prefix: u32) -> io::Result<(u8, usize)> {
    const HEADER_LEN: usize = 8 + 1 + 4 + 8;
    if bytes.len() < HEADER_LEN
        || (bytes[..8] != CHUNK_SHARD_MAGIC_V1 && bytes[..8] != CHUNK_SHARD_MAGIC_V2)
    {
        return Err(io::Error::other("invalid chunk catalog shard"));
    }
    let shard_bits = bytes[8];
    let mut at = 9;
    let prefix = take_u32(bytes, &mut at)?;
    if prefix != expected_prefix || validate_shard_bits(shard_bits).is_err() {
        return Err(io::Error::other("wrong chunk catalog shard prefix"));
    }
    let count = take_index_count(bytes, &mut at, CHUNK_LOCATION_LEN)?;
    let routing_len = if bytes[..8] == CHUNK_SHARD_MAGIC_V2 {
        8 + count.div_ceil(CHUNK_BLOCK_ENTRIES) * CHUNK_ROUTE_LEN
    } else {
        0
    };
    if at + count * CHUNK_LOCATION_LEN + routing_len != bytes.len() {
        return Err(io::Error::other("wrong chunk catalog shard length"));
    }
    Ok((shard_bits, count))
}

/// One block authenticated by a footer whose digest is committed in the map.
pub(super) struct ChunkBlock {
    pub(super) first: Digest,
    pub(super) last: Digest,
    pub(super) offset: u64,
    pub(super) length: u64,
    pub(super) digest: Digest,
}

pub(super) fn decode_chunk_routing(
    bytes: &[u8],
    reference: ShardRef,
) -> io::Result<Vec<ChunkBlock>> {
    let (_, footer_len) = reference
        .routing
        .ok_or_else(|| io::Error::other("missing chunk routing"))?;
    let mut at = 0;
    let count = take_index_count(bytes, &mut at, CHUNK_ROUTE_LEN)?;
    if count as u64 != reference.entries.div_ceil(CHUNK_BLOCK_ENTRIES as u64)
        || bytes.len() as u64 != footer_len
    {
        return Err(io::Error::other("invalid chunk routing count"));
    }
    let mut blocks = Vec::<ChunkBlock>::with_capacity(count);
    let mut offset = CHUNK_HEADER_LEN as u64;
    for ordinal in 0..count {
        let first = Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?;
        let last = Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?;
        let block = ChunkBlock {
            first,
            last,
            offset: take_index_u64(bytes, &mut at)?,
            length: take_index_u64(bytes, &mut at)?,
            digest: Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        };
        let entries = (reference.entries - ordinal as u64 * CHUNK_BLOCK_ENTRIES as u64)
            .min(CHUNK_BLOCK_ENTRIES as u64);
        if first > last
            || blocks.last().is_some_and(|previous| previous.last > first)
            || block.offset != offset
            || block.length != entries * CHUNK_LOCATION_LEN as u64
        {
            return Err(io::Error::other("invalid chunk routing bounds"));
        }
        offset = offset
            .checked_add(block.length)
            .ok_or_else(|| io::Error::other("chunk offset overflow"))?;
        blocks.push(block);
    }
    if at != bytes.len() || offset != reference.encoded_bytes - footer_len {
        return Err(io::Error::other("invalid chunk routing length"));
    }
    Ok(blocks)
}

pub(super) fn lookup_chunk_block(
    bytes: &[u8],
    bits: u8,
    prefix: u32,
    digest: &ChunkId,
) -> io::Result<Vec<Location>> {
    if !bytes.len().is_multiple_of(CHUNK_LOCATION_LEN) {
        return Err(io::Error::other("invalid chunk block length"));
    }
    if digest_prefix(digest.as_digest(), bits)? != prefix {
        return Err(io::Error::other("chunk lookup used the wrong shard"));
    }
    lookup_chunk_entries(bytes, digest)
}

fn decode_chunk_at(bytes: &[u8], ordinal: usize) -> io::Result<IndexedLocation> {
    decode_chunk_entry(&bytes[CHUNK_HEADER_LEN..], ordinal)
}

fn decode_chunk_entry(bytes: &[u8], ordinal: usize) -> io::Result<IndexedLocation> {
    let mut at = ordinal * CHUNK_LOCATION_LEN;
    let digest = ChunkId::new(
        Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?,
    );
    let pack = PackId::new(
        Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
            .map_err(io::Error::other)?,
    );
    Ok(IndexedLocation {
        digest,
        location: Location {
            pack,
            pack_len: take_index_u64(bytes, &mut at)?,
            offset: take_index_u64(bytes, &mut at)?,
            framed_len: take_index_u64(bytes, &mut at)?,
            uncompressed_len: take_index_u64(bytes, &mut at)?,
        },
    })
}

pub(super) fn decode_chunk_shard(
    bytes: &[u8],
    expected_prefix: u32,
) -> io::Result<Vec<IndexedLocation>> {
    let (shard_bits, count) = decode_chunk_header(bytes, expected_prefix)?;
    let mut entries = Vec::with_capacity(count);
    for ordinal in 0..count {
        let entry = decode_chunk_at(bytes, ordinal)?;
        if digest_prefix(entry.digest.as_digest(), shard_bits)? != expected_prefix
            || entries.last().is_some_and(|previous: &IndexedLocation| {
                previous.digest > entry.digest
                    || (previous.digest == entry.digest
                        && previous.location.pack >= entry.location.pack)
            })
        {
            return Err(io::Error::other("invalid chunk catalog shard contents"));
        }
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
pub(super) fn lookup_chunk_shard(
    bytes: &[u8],
    expected_prefix: u32,
    digest: &ChunkId,
) -> io::Result<Option<Location>> {
    let (shard_bits, count) = decode_chunk_header(bytes, expected_prefix)?;
    if digest_prefix(digest.as_digest(), shard_bits)? != expected_prefix {
        return Err(io::Error::other("chunk lookup used the wrong shard"));
    }
    let mut left = 0;
    let mut right = count;
    while left < right {
        let middle = left + (right - left) / 2;
        if decode_chunk_at(bytes, middle)?.digest < *digest {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    let found = (left < count)
        .then(|| decode_chunk_at(bytes, left))
        .transpose()?;
    Ok(found
        .filter(|entry| entry.digest == *digest)
        .map(|entry| entry.location))
}

pub(super) fn lookup_chunk_locations_shard(
    bytes: &[u8],
    expected_prefix: u32,
    digest: &ChunkId,
) -> io::Result<Vec<Location>> {
    let (shard_bits, count) = decode_chunk_header(bytes, expected_prefix)?;
    if digest_prefix(digest.as_digest(), shard_bits)? != expected_prefix {
        return Err(io::Error::other("chunk lookup used the wrong shard"));
    }
    lookup_chunk_entries(
        &bytes[CHUNK_HEADER_LEN..CHUNK_HEADER_LEN + count * CHUNK_LOCATION_LEN],
        digest,
    )
}

fn lookup_chunk_entries(bytes: &[u8], digest: &ChunkId) -> io::Result<Vec<Location>> {
    let count = bytes.len() / CHUNK_LOCATION_LEN;
    let mut left = 0;
    let mut right = count;
    while left < right {
        let middle = left + (right - left) / 2;
        if decode_chunk_entry(bytes, middle)?.digest < *digest {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    let mut locations = Vec::new();
    while left < count {
        let entry = decode_chunk_entry(bytes, left)?;
        if entry.digest != *digest {
            break;
        }
        locations.push(entry.location);
        left += 1;
    }
    Ok(locations)
}

/// Enumerate the live chunk IDs in one shard without materializing locations
/// from the rest of the catalog. A delta that changes a pack owns all of that
/// pack's entries, so locations from the immutable base are ignored wholesale.
pub(super) fn list_chunk_shard(
    bytes: &[u8],
    expected_prefix: u32,
    changed_packs: &HashSet<PackId>,
) -> io::Result<Vec<ChunkId>> {
    let (_, count) = decode_chunk_header(bytes, expected_prefix)?;
    let mut chunks = Vec::new();
    let mut ordinal = 0;
    while ordinal < count {
        let first = decode_chunk_at(bytes, ordinal)?;
        let digest = first.digest;
        let mut live = !changed_packs.contains(&first.location.pack);
        ordinal += 1;
        while ordinal < count {
            let entry = decode_chunk_at(bytes, ordinal)?;
            if entry.digest != digest {
                break;
            }
            live |= !changed_packs.contains(&entry.location.pack);
            ordinal += 1;
        }
        if live {
            chunks.push(digest);
        }
    }
    Ok(chunks)
}

fn encode_manifest_shard(shard_bits: u8, prefix: u32, manifests: &[BlobId]) -> io::Result<Bytes> {
    if manifests.windows(2).any(|pair| pair[0] >= pair[1])
        || manifests.iter().any(|manifest| {
            !digest_prefix(manifest.as_digest(), shard_bits).is_ok_and(|known| known == prefix)
        })
    {
        return Err(io::Error::other("invalid manifest catalog shard"));
    }
    let mut bytes = Vec::with_capacity(8 + 1 + 4 + 8 + manifests.len() * DIGEST_LEN);
    bytes.extend_from_slice(&MANIFEST_SHARD_MAGIC_V1);
    bytes.push(shard_bits);
    bytes.extend_from_slice(&prefix.to_le_bytes());
    bytes.extend_from_slice(&(manifests.len() as u64).to_le_bytes());
    for manifest in manifests {
        bytes.extend_from_slice(manifest.as_digest().as_bytes());
    }
    Ok(bytes.into())
}

fn encode_pack_shard(
    shard_bits: u8,
    prefix: u32,
    index: &Index,
    entries: usize,
) -> io::Result<Bytes> {
    validate_shard_bits(shard_bits)?;
    let known = index
        .packs
        .keys()
        .chain(index.superseded.iter())
        .copied()
        .collect::<HashSet<_>>();
    if known.len() != entries
        || known.iter().any(|pack| {
            !digest_prefix(pack.as_digest(), shard_bits).is_ok_and(|known| known == prefix)
        })
    {
        return Err(io::Error::other("invalid pack-state catalog shard"));
    }
    let checkpoint = encode_index_checkpoint(index, Digest::from(blake3::hash(SHARD_INVENTORY)))?;
    let mut bytes = Vec::with_capacity(8 + 1 + 4 + 8 + checkpoint.len());
    bytes.extend_from_slice(&PACK_SHARD_MAGIC_V1);
    bytes.push(shard_bits);
    bytes.extend_from_slice(&prefix.to_le_bytes());
    bytes.extend_from_slice(&(entries as u64).to_le_bytes());
    bytes.extend_from_slice(&checkpoint);
    Ok(bytes.into())
}

pub(super) fn decode_pack_shard(
    bytes: &[u8],
    expected_prefix: u32,
    expected_entries: u64,
) -> io::Result<Index> {
    const HEADER_LEN: usize = 8 + 1 + 4 + 8;
    if bytes.len() <= HEADER_LEN || bytes[..8] != PACK_SHARD_MAGIC_V1 {
        return Err(io::Error::other("invalid pack-state catalog shard"));
    }
    let shard_bits = bytes[8];
    let mut at = 9;
    let prefix = take_u32(bytes, &mut at)?;
    let entries = take_index_u64(bytes, &mut at)?;
    if prefix != expected_prefix
        || entries != expected_entries
        || validate_shard_bits(shard_bits).is_err()
    {
        return Err(io::Error::other("wrong pack-state catalog shard prefix"));
    }
    let index = decode_index_checkpoint_without_inventory(&bytes[at..])?;
    let known = index
        .packs
        .keys()
        .chain(index.superseded.iter())
        .copied()
        .collect::<HashSet<_>>();
    if known.len() as u64 != entries
        || known.iter().any(|pack| {
            !digest_prefix(pack.as_digest(), shard_bits).is_ok_and(|known| known == prefix)
        })
        || index.manifests.len() != 0
        || index.manifests_complete
    {
        return Err(io::Error::other(
            "invalid pack-state catalog shard contents",
        ));
    }
    Ok(index)
}

pub(super) fn manifest_shard_contains(
    bytes: &[u8],
    expected_prefix: u32,
    digest: &BlobId,
) -> io::Result<bool> {
    const HEADER_LEN: usize = 8 + 1 + 4 + 8;
    if bytes.len() < HEADER_LEN || bytes[..8] != MANIFEST_SHARD_MAGIC_V1 {
        return Err(io::Error::other("invalid manifest catalog shard"));
    }
    let shard_bits = bytes[8];
    let mut at = 9;
    if take_u32(bytes, &mut at)? != expected_prefix
        || digest_prefix(digest.as_digest(), shard_bits)? != expected_prefix
    {
        return Err(io::Error::other("manifest lookup used the wrong shard"));
    }
    let count = take_index_count(bytes, &mut at, DIGEST_LEN)?;
    if at + count * DIGEST_LEN != bytes.len() {
        return Err(io::Error::other("wrong manifest catalog shard length"));
    }
    let body = &bytes[at..];
    let wanted = digest.as_digest().as_bytes().as_slice();
    let mut left = 0;
    let mut right = count;
    while left < right {
        let middle = left + (right - left) / 2;
        let known = &body[middle * DIGEST_LEN..(middle + 1) * DIGEST_LEN];
        if known < wanted {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    Ok(left < count && &body[left * DIGEST_LEN..(left + 1) * DIGEST_LEN] == wanted)
}

pub(super) fn list_manifest_shard(bytes: &[u8], expected_prefix: u32) -> io::Result<Vec<BlobId>> {
    const HEADER_LEN: usize = 8 + 1 + 4 + 8;
    if bytes.len() < HEADER_LEN || bytes[..8] != MANIFEST_SHARD_MAGIC_V1 {
        return Err(io::Error::other("invalid manifest catalog shard"));
    }
    let shard_bits = bytes[8];
    let mut at = 9;
    if take_u32(bytes, &mut at)? != expected_prefix {
        return Err(io::Error::other("wrong manifest catalog shard prefix"));
    }
    let count = take_index_count(bytes, &mut at, DIGEST_LEN)?;
    if at + count * DIGEST_LEN != bytes.len() {
        return Err(io::Error::other("wrong manifest catalog shard length"));
    }
    let mut manifests = Vec::with_capacity(count);
    for _ in 0..count {
        let manifest = BlobId::new(
            Digest::try_from(take_index_bytes(bytes, &mut at, DIGEST_LEN)?)
                .map_err(io::Error::other)?,
        );
        if digest_prefix(manifest.as_digest(), shard_bits)? != expected_prefix
            || manifests
                .last()
                .is_some_and(|previous| *previous >= manifest)
        {
            return Err(io::Error::other("invalid manifest catalog shard contents"));
        }
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn shard_ref(prefix: u32, entries: usize, bytes: &Bytes) -> ShardRef {
    ShardRef {
        prefix,
        digest: Digest::from(blake3::hash(bytes)),
        entries: entries as u64,
        encoded_bytes: bytes.len() as u64,
        routing: if bytes[..8] == CHUNK_SHARD_MAGIC_V2 {
            let footer = &bytes[CHUNK_HEADER_LEN + entries * CHUNK_LOCATION_LEN..];
            Some((Digest::from(blake3::hash(footer)), footer.len() as u64))
        } else {
            None
        },
    }
}

pub(super) fn encode_chunk_shard_object(
    shard_bits: u8,
    prefix: u32,
    entries: &mut [IndexedLocation],
) -> io::Result<(ShardRef, Bytes)> {
    let count = entries.len();
    let bytes = encode_chunk_shard(shard_bits, prefix, entries)?;
    Ok((shard_ref(prefix, count, &bytes), bytes))
}

pub(super) fn encode_manifest_shard_object(
    shard_bits: u8,
    prefix: u32,
    manifests: &[BlobId],
) -> io::Result<(ShardRef, Bytes)> {
    let bytes = encode_manifest_shard(shard_bits, prefix, manifests)?;
    Ok((shard_ref(prefix, manifests.len(), &bytes), bytes))
}

pub(super) fn encode_pack_shard_object(
    shard_bits: u8,
    prefix: u32,
    index: &Index,
) -> io::Result<(ShardRef, Bytes)> {
    let known = index
        .packs
        .keys()
        .chain(index.superseded.iter())
        .copied()
        .collect::<HashSet<_>>()
        .len();
    let bytes = encode_pack_shard(shard_bits, prefix, index, known)?;
    Ok((shard_ref(prefix, known, &bytes), bytes))
}

pub(super) fn encode_index_shards(index: &Index, shard_bits: u8) -> io::Result<EncodedShards> {
    validate_shard_bits(shard_bits)?;
    let mut objects = Vec::new();
    let mut chunk_groups: BTreeMap<u32, Vec<IndexedLocation>> = BTreeMap::new();
    for (pack, entries) in &index.packs {
        if index.superseded.contains(pack) {
            continue;
        }
        let pack_len = index.pack_lengths.get(pack).copied().unwrap_or_default();
        for entry in entries {
            if index
                .tombstoned
                .get(pack)
                .is_some_and(|dead| dead.contains(&entry.digest))
            {
                continue;
            }
            let prefix = digest_prefix(entry.digest.as_digest(), shard_bits)?;
            chunk_groups
                .entry(prefix)
                .or_default()
                .push(IndexedLocation {
                    digest: entry.digest,
                    location: Location {
                        pack: *pack,
                        pack_len,
                        offset: entry.offset,
                        framed_len: entry.framed_len,
                        uncompressed_len: entry.uncompressed_len,
                    },
                });
        }
    }
    let mut chunk_refs = Vec::with_capacity(chunk_groups.len());
    for (prefix, mut entries) in chunk_groups {
        let count = entries.len();
        let bytes = encode_chunk_shard(shard_bits, prefix, &mut entries)?;
        let reference = shard_ref(prefix, count, &bytes);
        objects.push((reference.digest, bytes));
        chunk_refs.push(reference);
    }

    let mut manifest_groups: BTreeMap<u32, Vec<BlobId>> = BTreeMap::new();
    for manifest in index.manifests.sorted_ids() {
        manifest_groups
            .entry(digest_prefix(manifest.as_digest(), shard_bits)?)
            .or_default()
            .push(manifest);
    }
    let mut manifest_refs = Vec::with_capacity(manifest_groups.len());
    for (prefix, manifests) in manifest_groups {
        let bytes = encode_manifest_shard(shard_bits, prefix, &manifests)?;
        let reference = shard_ref(prefix, manifests.len(), &bytes);
        objects.push((reference.digest, bytes));
        manifest_refs.push(reference);
    }

    let mut pack_groups: BTreeMap<u32, Vec<PackId>> = BTreeMap::new();
    let pack_ids = index
        .packs
        .keys()
        .chain(index.superseded.iter())
        .copied()
        .collect::<HashSet<_>>();
    for pack in pack_ids {
        pack_groups
            .entry(digest_prefix(pack.as_digest(), shard_bits)?)
            .or_default()
            .push(pack);
    }
    let mut pack_refs = Vec::with_capacity(pack_groups.len());
    for (prefix, mut packs) in pack_groups {
        packs.sort_unstable();
        let mut shard = Index::default();
        for pack in &packs {
            if let Some(entries) = index.packs.get(pack) {
                shard.add_pack_metadata(
                    *pack,
                    index.pack_lengths.get(pack).copied().unwrap_or_default(),
                    entries.clone(),
                );
                if let Some(dead) = index.tombstoned.get(pack) {
                    shard.tombstoned.insert(*pack, dead.clone());
                }
                if let Some(records) = index.tombstone_records.get(pack) {
                    shard.tombstone_records.insert(*pack, records.clone());
                }
            }
            if index.superseded.contains(pack) {
                shard.superseded.insert(*pack);
            }
        }
        let bytes = encode_pack_shard(shard_bits, prefix, &shard, packs.len())?;
        let reference = shard_ref(prefix, packs.len(), &bytes);
        objects.push((reference.digest, bytes));
        pack_refs.push(reference);
    }

    let map = encode_shard_map(&ShardMap {
        shard_bits,
        chunks: chunk_refs,
        manifests: manifest_refs,
        packs: pack_refs,
        run_routing: BTreeMap::new(),
    })?;
    Ok(EncodedShards {
        map_digest: Digest::from(blake3::hash(&map)),
        map,
        objects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct DeltaOverlay {
        local: Index,
        changed_packs: HashSet<PackId>,
        removed_manifests: HashSet<BlobId>,
    }

    impl DeltaOverlay {
        fn apply(&mut self, bytes: &[u8]) -> io::Result<()> {
            let delta = super::super::delta::decode_index_delta(bytes)?;
            self.changed_packs
                .extend(delta.removed_packs.iter().copied());
            self.changed_packs.extend(delta.patch.packs.keys().copied());
            self.changed_packs
                .extend(delta.patch.superseded.iter().copied());
            self.removed_manifests
                .extend(delta.removed_manifests.iter().copied());
            for manifest in delta.patch.manifests.sorted_ids() {
                self.removed_manifests.remove(&manifest);
            }
            super::super::delta::apply_decoded_index_delta(&mut self.local, delta);
            Ok(())
        }

        fn location(&self, digest: &ChunkId, base: Option<Location>) -> Option<Location> {
            self.local
                .chunks
                .get(digest)
                .or_else(|| base.filter(|location| !self.changed_packs.contains(&location.pack)))
        }

        fn manifest_contains(&self, digest: &BlobId, base_contains: bool) -> bool {
            self.local.manifests.contains(digest)
                || (!self.removed_manifests.contains(digest) && base_contains)
        }
    }

    fn entry(label: &[u8], offset: u64) -> PackEntry {
        PackEntry {
            digest: ChunkId::new(blake3::hash(label).into()),
            offset,
            framed_len: 32,
            uncompressed_len: 64,
        }
    }

    fn object<'a>(encoded: &'a EncodedShards, digest: &Digest) -> &'a Bytes {
        &encoded
            .objects
            .iter()
            .find(|(known, _)| known == digest)
            .expect("referenced shard object")
            .1
    }

    fn base_location(
        encoded: &EncodedShards,
        map: &ShardMap,
        digest: &ChunkId,
    ) -> Option<Location> {
        let prefix = digest_prefix(digest.as_digest(), map.shard_bits).unwrap();
        map.chunks
            .binary_search_by_key(&prefix, |shard| shard.prefix)
            .ok()
            .map(|at| &map.chunks[at])
            .map(|shard| {
                lookup_chunk_shard(object(encoded, &shard.digest), prefix, digest).unwrap()
            })
            .unwrap_or(None)
    }

    fn base_manifest_contains(encoded: &EncodedShards, map: &ShardMap, digest: &BlobId) -> bool {
        let prefix = digest_prefix(digest.as_digest(), map.shard_bits).unwrap();
        map.manifests
            .binary_search_by_key(&prefix, |shard| shard.prefix)
            .ok()
            .map(|at| &map.manifests[at])
            .is_some_and(|shard| {
                manifest_shard_contains(object(encoded, &shard.digest), prefix, digest).unwrap()
            })
    }

    #[test]
    fn routed_chunk_blocks_preserve_duplicates_and_legacy_shards() {
        let digest = ChunkId::new(Digest::from([0; 32]));
        let mut entries: Vec<_> = (0..1025u64)
            .map(|index| {
                let mut pack = [0; 32];
                pack[24..].copy_from_slice(&index.to_be_bytes());
                IndexedLocation {
                    digest,
                    location: Location {
                        pack: PackId::new(Digest::from(pack)),
                        pack_len: 100,
                        offset: 0,
                        framed_len: 32,
                        uncompressed_len: 64,
                    },
                }
            })
            .collect();
        let (reference, bytes) = encode_chunk_shard_object(1, 0, &mut entries).unwrap();
        let (footer_digest, footer_len) = reference.routing.unwrap();
        let footer = &bytes[bytes.len() - footer_len as usize..];
        assert_eq!(Digest::from(blake3::hash(footer)), footer_digest);
        let blocks = decode_chunk_routing(footer, reference).unwrap();
        assert_eq!(blocks.len(), 2);
        let mut locations = Vec::new();
        for block in blocks {
            let block_bytes = &bytes[block.offset as usize..(block.offset + block.length) as usize];
            assert_eq!(Digest::from(blake3::hash(block_bytes)), block.digest);
            locations.extend(lookup_chunk_block(block_bytes, 1, 0, &digest).unwrap());
        }
        assert_eq!(locations.len(), 1025);
        assert_eq!(decode_chunk_shard(&bytes, 0).unwrap().len(), 1025);
        let mut legacy = bytes[..bytes.len() - footer_len as usize].to_vec();
        legacy[..8].copy_from_slice(&CHUNK_SHARD_MAGIC_V1);
        assert_eq!(
            lookup_chunk_locations_shard(&legacy, 0, &digest).unwrap(),
            locations
        );
        let mut truncated = footer.to_vec();
        truncated.pop();
        assert!(decode_chunk_routing(&truncated, reference).is_err());
    }

    #[test]
    fn legacy_v2_shard_maps_remain_readable() {
        let mut body = vec![1];
        body.extend_from_slice(&0u64.to_le_bytes()); // chunks
        body.extend_from_slice(&0u64.to_le_bytes()); // manifests
        body.extend_from_slice(&0u64.to_le_bytes()); // packs
        body.extend_from_slice(&0u64.to_le_bytes()); // runs
        let mut hash = blake3::Hasher::new();
        hash.update(b"casita catalog shard map v2\0");
        hash.update(&body);
        let mut encoded = SHARD_MAP_MAGIC_V2.to_vec();
        encoded.extend_from_slice(hash.finalize().as_bytes());
        encoded.extend_from_slice(&body);
        let decoded = decode_shard_map(&encoded).unwrap();
        assert_eq!(decoded.shard_bits, 1);
        assert!(decoded.chunks.is_empty());
        assert_eq!(
            decode_shard_map(&encode_shard_map(&decoded).unwrap()).unwrap(),
            decoded
        );
    }

    #[test]
    fn shard_map_and_all_three_tables_are_exact_and_content_addressed() {
        let first = PackId::new(blake3::hash(b"first shard pack").into());
        let removed = PackId::new(blake3::hash(b"removed shard pack").into());
        let entries = vec![
            entry(b"live shard chunk", 0),
            entry(b"dead shard chunk", 32),
        ];
        let live = entries[0].digest;
        let dead = entries[1].digest;
        let manifest = BlobId::new(blake3::hash(b"sharded manifest").into());
        let mut index = Index::default();
        index.add_pack(first, 256, entries.clone());
        index.tombstoned.insert(first, HashSet::from([dead]));
        index.tombstone_records.insert(
            first,
            HashSet::from([Digest::from(blake3::hash(b"shard tombstone"))]),
        );
        index.superseded.insert(removed);
        index.manifests.insert(manifest);
        index.manifests_complete = true;
        index.rebuild_chunks();

        let encoded = encode_index_shards(&index, 12).unwrap();
        assert_eq!(Digest::from(blake3::hash(&encoded.map)), encoded.map_digest);
        let map = decode_shard_map(&encoded.map).unwrap();
        assert_eq!(map.shard_bits, 12);
        assert_eq!(map.chunks.iter().map(|shard| shard.entries).sum::<u64>(), 1);
        assert_eq!(
            map.manifests.iter().map(|shard| shard.entries).sum::<u64>(),
            1
        );
        assert_eq!(map.packs.iter().map(|shard| shard.entries).sum::<u64>(), 2);
        for shard in map.chunks.iter().chain(&map.manifests).chain(&map.packs) {
            let bytes = object(&encoded, &shard.digest);
            assert_eq!(Digest::from(blake3::hash(bytes)), shard.digest);
            assert_eq!(bytes.len() as u64, shard.encoded_bytes);
        }
        let mut decoded_pack_count = 0;
        for shard in &map.packs {
            let decoded =
                decode_pack_shard(object(&encoded, &shard.digest), shard.prefix, shard.entries)
                    .unwrap();
            decoded_pack_count += decoded.packs.len() + decoded.superseded.len();
            if decoded.packs.contains_key(&first) {
                assert_eq!(decoded.tombstoned[&first], HashSet::from([dead]));
                assert_eq!(decoded.tombstone_records[&first].len(), 1);
            }
        }
        assert_eq!(decoded_pack_count, 2);

        let live_prefix = digest_prefix(live.as_digest(), 12).unwrap();
        let live_shard = map
            .chunks
            .iter()
            .find(|shard| shard.prefix == live_prefix)
            .unwrap();
        assert_eq!(
            lookup_chunk_shard(object(&encoded, &live_shard.digest), live_prefix, &live)
                .unwrap()
                .unwrap()
                .pack,
            first
        );
        let dead_prefix = digest_prefix(dead.as_digest(), 12).unwrap();
        let dead_location = map
            .chunks
            .iter()
            .find(|shard| shard.prefix == dead_prefix)
            .map(|shard| {
                lookup_chunk_shard(object(&encoded, &shard.digest), dead_prefix, &dead).unwrap()
            })
            .unwrap_or(None);
        assert!(dead_location.is_none());

        let manifest_prefix = digest_prefix(manifest.as_digest(), 12).unwrap();
        let manifest_shard = map
            .manifests
            .iter()
            .find(|shard| shard.prefix == manifest_prefix)
            .unwrap();
        assert!(
            manifest_shard_contains(
                object(&encoded, &manifest_shard.digest),
                manifest_prefix,
                &manifest,
            )
            .unwrap()
        );
    }

    #[test]
    fn shard_codecs_reject_corruption_wrong_prefixes_and_unsorted_maps() {
        let digest = Digest::from(blake3::hash(b"map shard"));
        let mut map = ShardMap {
            shard_bits: 12,
            chunks: vec![
                ShardRef {
                    routing: None,
                    prefix: 2,
                    digest,
                    entries: 1,
                    encoded_bytes: 100,
                },
                ShardRef {
                    routing: None,
                    prefix: 1,
                    digest,
                    entries: 1,
                    encoded_bytes: 100,
                },
            ],
            manifests: Vec::new(),
            packs: Vec::new(),
            run_routing: BTreeMap::new(),
        };
        assert!(encode_shard_map(&map).is_err());
        map.chunks.sort_unstable_by_key(|shard| shard.prefix);
        let mut encoded = encode_shard_map(&map).unwrap().to_vec();
        *encoded.last_mut().unwrap() ^= 1;
        assert!(decode_shard_map(&encoded).is_err());

        let chunk = ChunkId::new(blake3::hash(b"wrong-prefix chunk").into());
        let prefix = digest_prefix(chunk.as_digest(), 12).unwrap();
        let mut entries = [IndexedLocation {
            digest: chunk,
            location: Location {
                pack: PackId::new(blake3::hash(b"wrong-prefix pack").into()),
                pack_len: 100,
                offset: 0,
                framed_len: 10,
                uncompressed_len: 20,
            },
        }];
        let shard = encode_chunk_shard(12, prefix, &mut entries).unwrap();
        assert!(lookup_chunk_shard(&shard, prefix ^ 1, &chunk).is_err());

        let pack = entries[0].location.pack;
        let pack_prefix = digest_prefix(pack.as_digest(), 12).unwrap();
        let mut pack_index = Index::default();
        pack_index.add_pack_metadata(pack, 100, vec![entry(b"pack shard chunk", 0)]);
        let pack_shard = encode_pack_shard(12, pack_prefix, &pack_index, 1).unwrap();
        assert!(decode_pack_shard(&pack_shard, pack_prefix ^ 1, 1).is_err());
        assert!(decode_pack_shard(&pack_shard, pack_prefix, 2).is_err());
    }

    #[test]
    fn shard_width_tracks_the_encoded_byte_target_including_500_tb() {
        assert_eq!(
            recommended_shard_bits(65_536, DEFAULT_SHARD_TARGET_BYTES).unwrap(),
            1
        );
        assert_eq!(
            recommended_shard_bits(1_000_000, DEFAULT_SHARD_TARGET_BYTES).unwrap(),
            2
        );
        let chunks_at_500_tb = 500_000_000_000_000_u64.div_ceil(256 * 1024);
        assert_eq!(chunks_at_500_tb, 1_907_348_633);
        assert_eq!(
            recommended_shard_bits(chunks_at_500_tb, DEFAULT_SHARD_TARGET_BYTES).unwrap(),
            13
        );
        assert!(recommended_shard_bits(1, 0).is_err());
    }

    #[test]
    fn exact_delta_overlay_hides_unloaded_base_records_and_newer_operations_win() {
        let unchanged_pack = PackId::new(blake3::hash(b"unchanged overlay pack").into());
        let changed_pack = PackId::new(blake3::hash(b"changed overlay pack").into());
        let unchanged_chunk = entry(b"unchanged overlay chunk", 0);
        let removed_chunk = entry(b"removed overlay chunk", 0);
        let replacement_chunk = entry(b"replacement overlay chunk", 0);
        let unchanged_manifest = BlobId::new(blake3::hash(b"unchanged overlay manifest").into());
        let removed_manifest = BlobId::new(blake3::hash(b"removed overlay manifest").into());
        let added_manifest = BlobId::new(blake3::hash(b"added overlay manifest").into());

        let mut base = Index::default();
        base.add_pack(unchanged_pack, 128, vec![unchanged_chunk]);
        base.add_pack(changed_pack, 128, vec![removed_chunk]);
        base.manifests
            .extend([unchanged_manifest, removed_manifest]);
        base.manifests_complete = true;
        base.rebuild_chunks();
        let encoded = encode_index_shards(&base, 4).unwrap();
        let map = decode_shard_map(&encoded.map).unwrap();

        let mut next = base.clone();
        next.remove_pack(changed_pack);
        next.superseded.remove(&changed_pack);
        next.add_pack(changed_pack, 256, vec![replacement_chunk]);
        next.manifests.remove(&removed_manifest);
        next.manifests.insert(added_manifest);
        next.rebuild_chunks();

        let mut overlay = DeltaOverlay::default();
        overlay
            .apply(&super::super::delta::encode_index_delta(&base, &next).unwrap())
            .unwrap();
        assert!(base_location(&encoded, &map, &removed_chunk.digest).is_some());
        assert!(
            overlay
                .location(
                    &removed_chunk.digest,
                    base_location(&encoded, &map, &removed_chunk.digest),
                )
                .is_none()
        );
        assert_eq!(
            overlay
                .location(&replacement_chunk.digest, None)
                .unwrap()
                .pack,
            changed_pack
        );
        assert_eq!(
            overlay
                .location(
                    &unchanged_chunk.digest,
                    base_location(&encoded, &map, &unchanged_chunk.digest),
                )
                .unwrap()
                .pack,
            unchanged_pack
        );
        assert!(!overlay.manifest_contains(
            &removed_manifest,
            base_manifest_contains(&encoded, &map, &removed_manifest),
        ));
        assert!(overlay.manifest_contains(&added_manifest, false));
        assert!(overlay.manifest_contains(
            &unchanged_manifest,
            base_manifest_contains(&encoded, &map, &unchanged_manifest),
        ));

        let mut final_index = next.clone();
        final_index.manifests.remove(&added_manifest);
        final_index.manifests.insert(removed_manifest);
        overlay
            .apply(&super::super::delta::encode_index_delta(&next, &final_index).unwrap())
            .unwrap();
        assert!(!overlay.manifest_contains(&added_manifest, false));
        assert!(overlay.manifest_contains(
            &removed_manifest,
            base_manifest_contains(&encoded, &map, &removed_manifest),
        ));
    }
}
