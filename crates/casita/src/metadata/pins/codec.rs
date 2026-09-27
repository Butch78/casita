//! Bounded, checksummed persistence for pin inventories.
use super::*;

pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;
const READER_MAGIC: &[u8; 8] = b"CASPIN04";
const MAGIC: &[u8; 8] = b"CASPIN03";
const COLLECTOR_MAGIC: &[u8; 8] = b"CASPIN02";
const LEGACY_MAGIC: &[u8; 8] = b"CASPIN01";

fn invalid() -> MetadataError {
    MetadataError::Corruption("invalid pin inventory".into())
}

pub(super) fn encode(state: &PinInventory) -> Result<Vec<u8>, MetadataError> {
    let readers = !state.reader_owners.is_empty() || state.reader_revision_ceiling != 0;
    let mut out = Encoder(if readers { READER_MAGIC } else { MAGIC }.to_vec());
    out.0.extend_from_slice(&state.revision.to_le_bytes());
    out.count(state.pins.len())?;
    for (token, pin) in &state.pins {
        pin.validate()?;
        out.0.extend_from_slice(&token.0);
        match &pin.scope {
            PinScope::Snapshot { generation } => {
                out.0.push(4);
                out.0.extend_from_slice(&generation.to_le_bytes());
            }
            PinScope::Closures(roots) => {
                out.0.push(1);
                out.count(roots.len())?;
                for key in roots {
                    out.bytes(&key.encode())?;
                }
            }
            PinScope::Staging => out.0.push(2),
            PinScope::Metadata => out.0.push(3),
        }
        out.0.push(u8::from(pin.catalog.is_some()));
        if let Some(catalog) = &pin.catalog {
            out.bytes(catalog)?;
        }
        out.resources(&pin.resources)?;
    }
    out.count(state.deletions.len())?;
    for (token, resources) in &state.deletions {
        out.0.extend_from_slice(&token.0);
        out.resources(resources)?;
    }
    out.0.push(u8::from(state.logical_prune.is_some()));
    if let Some(token) = &state.logical_prune {
        out.0.extend_from_slice(&token.0);
    }
    out.0.push(u8::from(state.collector.is_some()));
    if let Some(token) = &state.collector {
        out.0.extend_from_slice(&token.0);
    }
    out.count(state.retired.len())?;
    for token in &state.retired {
        out.0.extend_from_slice(&token.0);
    }
    if readers {
        out.0
            .extend_from_slice(&state.reader_revision_ceiling.to_le_bytes());
        out.count(state.reader_owners.len())?;
        for owner in &state.reader_owners {
            out.0.extend_from_slice(&owner.0);
        }
    }
    if out.0.len() > MAX_BYTES - 32 {
        return Err(invalid());
    }
    let checksum = blake3::hash(&out.0);
    out.0.extend_from_slice(checksum.as_bytes());
    Ok(out.0)
}

struct Encoder(Vec<u8>);
impl Encoder {
    fn count(&mut self, count: usize) -> Result<(), MetadataError> {
        let count = u32::try_from(count).map_err(|_| invalid())?;
        self.0.extend_from_slice(&count.to_le_bytes());
        Ok(())
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), MetadataError> {
        if self.0.len().saturating_add(bytes.len()).saturating_add(4) > MAX_BYTES - 32 {
            return Err(invalid());
        }
        self.count(bytes.len())?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn resources(&mut self, resources: &BTreeSet<PinResource>) -> Result<(), MetadataError> {
        self.count(resources.len())?;
        for resource in resources {
            match resource {
                PinResource::Blob(id) => {
                    self.0.push(0);
                    self.0.extend_from_slice(id.as_digest().as_bytes());
                }
                PinResource::Chunk(id) => {
                    self.0.push(1);
                    self.0.extend_from_slice(id.as_digest().as_bytes());
                }
                PinResource::StorageObject(path) => {
                    self.0.push(2);
                    self.bytes(path.as_bytes())?;
                }
                PinResource::Object(key) => {
                    self.0.push(3);
                    self.bytes(&key.encode())?;
                }
                PinResource::Catalog(catalog) => {
                    self.0.push(4);
                    self.bytes(catalog)?;
                }
                PinResource::MetadataObject(path) => {
                    self.0.push(5);
                    self.bytes(path.as_bytes())?;
                }
            }
            if self.0.len() > MAX_BYTES - 32 {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

pub(super) fn decode(bytes: &[u8]) -> Result<PinInventory, MetadataError> {
    if bytes.len() < 32 || bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    let (bytes, checksum) = bytes.split_at(bytes.len() - 32);
    if blake3::hash(bytes).as_bytes() != checksum {
        return Err(invalid());
    }
    let mut input = Decoder(bytes);
    let magic = input.take(8)?;
    if magic != READER_MAGIC && magic != MAGIC && magic != COLLECTOR_MAGIC && magic != LEGACY_MAGIC
    {
        return Err(invalid());
    }
    let revision = u64::from_le_bytes(input.take(8)?.try_into().map_err(|_| invalid())?);
    let mut pins = BTreeMap::new();
    for _ in 0..input.count()? {
        let token = input.token()?;
        let scope = match input.byte()? {
            0 => PinScope::Snapshot {
                generation: u64::MAX,
            },
            4 if magic == MAGIC || magic == READER_MAGIC => PinScope::Snapshot {
                generation: u64::from_le_bytes(input.take(8)?.try_into().map_err(|_| invalid())?),
            },
            1 => {
                let mut roots = BTreeSet::new();
                for _ in 0..input.count()? {
                    let key = ObjectKey::decode(input.bytes()?).map_err(|_| invalid())?;
                    if !roots.insert(key) {
                        return Err(invalid());
                    }
                }
                PinScope::Closures(roots)
            }
            2 => PinScope::Staging,
            3 => PinScope::Metadata,
            _ => return Err(invalid()),
        };
        let catalog = match input.byte()? {
            0 => None,
            1 => Some(input.bytes()?.to_vec()),
            _ => return Err(invalid()),
        };
        let resources = input.resources()?;
        let pin = DataPin {
            scope,
            catalog,
            resources,
        };
        pin.validate()?;
        if pins.insert(token, pin).is_some() {
            return Err(invalid());
        }
    }
    let mut deletions = BTreeMap::new();
    for _ in 0..input.count()? {
        let token = input.token()?;
        if pins.contains_key(&token) || deletions.insert(token, input.resources()?).is_some() {
            return Err(invalid());
        }
    }
    let logical_prune = match input.byte()? {
        0 => None,
        1 => Some(input.token()?),
        _ => return Err(invalid()),
    };
    let (collector, retired) = if magic != LEGACY_MAGIC {
        let collector = match input.byte()? {
            0 => None,
            1 => Some(input.token()?),
            _ => return Err(invalid()),
        };
        let mut retired = BTreeSet::new();
        for _ in 0..input.count()? {
            let token = input.token()?;
            if !pins.contains_key(&token) || !retired.insert(token) {
                return Err(invalid());
            }
        }
        if collector.is_none() && !retired.is_empty() {
            return Err(invalid());
        }
        if let Some(token) = &collector
            && (pins.contains_key(token)
                || deletions.contains_key(token)
                || logical_prune.as_ref() == Some(token))
        {
            return Err(invalid());
        }
        (collector, retired)
    } else {
        (None, BTreeSet::new())
    };
    let (reader_revision_ceiling, reader_owners) = if magic == READER_MAGIC {
        let ceiling = u64::from_le_bytes(input.take(8)?.try_into().map_err(|_| invalid())?);
        let mut owners = BTreeSet::new();
        for _ in 0..input.count()? {
            if !owners.insert(input.token()?) {
                return Err(invalid());
            }
        }
        (ceiling, owners)
    } else {
        (0, BTreeSet::new())
    };
    if !input.0.is_empty() {
        return Err(invalid());
    }
    if let Some(token) = &logical_prune
        && (pins.contains_key(token) || deletions.contains_key(token))
    {
        return Err(invalid());
    }
    let mut claimed = BTreeSet::new();
    for resources in deletions.values() {
        for resource in resources {
            if !claimed.insert(resource) {
                return Err(invalid());
            }
        }
        if pins
            .values()
            .any(|pin| !pin.resources.is_disjoint(resources))
        {
            return Err(invalid());
        }
    }
    Ok(PinInventory {
        revision,
        reader_owners,
        reader_revision_ceiling,
        pins,
        deletions,
        logical_prune,
        collector,
        retired,
    })
}

struct Decoder<'a>(&'a [u8]);
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MetadataError> {
        if n > self.0.len() {
            return Err(invalid());
        }
        let (part, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(part)
    }
    fn byte(&mut self) -> Result<u8, MetadataError> {
        Ok(self.take(1)?[0])
    }
    fn count(&mut self) -> Result<usize, MetadataError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| invalid())?) as usize)
    }
    fn bytes(&mut self) -> Result<&'a [u8], MetadataError> {
        let n = self.count()?;
        self.take(n)
    }
    fn token(&mut self) -> Result<PinToken, MetadataError> {
        Ok(PinToken(self.take(32)?.try_into().map_err(|_| invalid())?))
    }
    fn resources(&mut self) -> Result<BTreeSet<PinResource>, MetadataError> {
        let mut resources = BTreeSet::new();
        for _ in 0..self.count()? {
            let value = match self.byte()? {
                0 => PinResource::Blob(BlobId::new(
                    crate::Digest::try_from(self.take(32)?).map_err(|_| invalid())?,
                )),
                1 => PinResource::Chunk(ChunkId::new(
                    crate::Digest::try_from(self.take(32)?).map_err(|_| invalid())?,
                )),
                2 => PinResource::StorageObject(
                    std::str::from_utf8(self.bytes()?)
                        .map_err(|_| invalid())?
                        .to_owned(),
                ),
                3 => PinResource::Object(ObjectKey::decode(self.bytes()?).map_err(|_| invalid())?),
                4 => PinResource::Catalog(self.bytes()?.to_vec()),
                5 => PinResource::MetadataObject(
                    std::str::from_utf8(self.bytes()?)
                        .map_err(|_| invalid())?
                        .to_owned(),
                ),
                _ => return Err(invalid()),
            };
            if !resources.insert(value) {
                return Err(invalid());
            }
        }
        Ok(resources)
    }
}

/// Wire sizes used by the local cache, visiting only the requested records.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn resources_len(items: &BTreeSet<PinResource>) -> usize {
    4 + items
        .iter()
        .map(|item| {
            1 + match item {
                PinResource::Blob(_) | PinResource::Chunk(_) => 32,
                PinResource::StorageObject(path) | PinResource::MetadataObject(path) => {
                    4 + path.len()
                }
                PinResource::Catalog(bytes) => 4 + bytes.len(),
                PinResource::Object(key) => 4 + key.encode().len(),
            }
        })
        .sum::<usize>()
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn pin_len(pin: &DataPin) -> usize {
    32 + 1
        + match &pin.scope {
            PinScope::Snapshot { .. } => 8,
            PinScope::Closures(roots) => {
                4 + roots
                    .iter()
                    .map(|key| 4 + key.encode().len())
                    .sum::<usize>()
            }
            PinScope::Staging | PinScope::Metadata => 0,
        }
        + 1
        + pin.catalog.as_ref().map_or(0, |catalog| 4 + catalog.len())
        + resources_len(&pin.resources)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn globals_len(state: &PinInventory) -> usize {
    62 + usize::from(state.logical_prune.is_some()) * 32
        + usize::from(state.collector.is_some()) * 32
        + state.retired.len() * 32
        + if !state.reader_owners.is_empty() || state.reader_revision_ceiling != 0 {
            12 + state.reader_owners.len() * 32
        } else {
            0
        }
}
/// Exact encoded size without allocating a second serialized inventory.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn encoded_len(state: &PinInventory) -> Result<usize, MetadataError> {
    let size = globals_len(state)
        + state.pins.values().map(pin_len).sum::<usize>()
        + state
            .deletions
            .values()
            .map(|resources| 32 + resources_len(resources))
            .sum::<usize>();
    if size > MAX_BYTES {
        return Err(invalid());
    }
    Ok(size)
}
