//! Frozen CIDv1 object formats used to prove generic graph behavior.
//!
//! `ipld.raw.v1` is terminal and uses the standard raw multicodec. The linked
//! format uses a Casita-private multicodec and a deliberately small canonical
//! binary payload: a sorted set of CID links followed by opaque application
//! bytes. Both use BLAKE3-256 multihashes.

use std::sync::Arc;

use bytes::Bytes;

use crate::format::{FormatError, FormatLimits, ObjectFormat, VerificationContext, VerifiedObject};
use crate::object::{NamespaceId, ObjectKey};
use crate::{Digest, ObjectKeyError};

/// Terminal raw IPLD namespace.
pub const IPLD_RAW_NAMESPACE: &str = "ipld.raw.v1";
/// Canonical linked IPLD namespace.
pub const IPLD_LINKED_NAMESPACE: &str = "ipld.linked.v1";

/// CIDv1 raw multicodec.
pub const RAW_CODEC: u64 = 0x55;
/// Private-use multicodec frozen for the linked v1 payload.
pub const CASITA_LINKED_CODEC: u64 = 0x30_0001;
/// BLAKE3-256 multihash code.
pub const BLAKE3_256_MULTIHASH: u64 = 0x1e;

const LINKED_MAGIC: &[u8] = b"casita-ipld-linked-v1\0";

/// A canonical CIDv1 using a BLAKE3-256 multihash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IpldCid {
    bytes: Bytes,
    codec: u64,
    digest: Digest,
}

/// Why CID or linked-IPLD bytes are invalid.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IpldError {
    /// A varint is truncated, overlong, overflowing, or noncanonical.
    #[error("invalid canonical unsigned varint")]
    InvalidVarint,
    /// Only CID version 1 is accepted.
    #[error("unsupported CID version {0}")]
    UnsupportedCidVersion(u64),
    /// Only the two frozen codecs are accepted.
    #[error("unsupported CID codec {0:#x}")]
    UnsupportedCodec(u64),
    /// Only BLAKE3-256 multihashes are accepted.
    #[error("unsupported multihash code {0:#x}")]
    UnsupportedMultihash(u64),
    /// The multihash digest is not exactly 32 bytes.
    #[error("BLAKE3 multihash length is {0}, expected 32")]
    InvalidDigestLength(u64),
    /// Bytes remain after the one canonical CID.
    #[error("trailing bytes after CID")]
    TrailingCidBytes,
    /// The native CID codec does not match the selected namespace.
    #[error("CID codec {actual:#x} does not match expected codec {expected:#x}")]
    CodecMismatch {
        /// Codec selected by the object namespace.
        expected: u64,
        /// Codec carried by the CID.
        actual: u64,
    },
    /// The payload hash does not match its CID multihash.
    #[error("CID digest mismatch: expected {expected}, observed {actual}")]
    DigestMismatch {
        /// Digest in the CID.
        expected: Digest,
        /// Digest of the complete payload.
        actual: Digest,
    },
    /// The linked payload has the wrong fixed prefix.
    #[error("invalid linked-IPLD magic")]
    InvalidLinkedMagic,
    /// A length or count cannot fit the current address space.
    #[error("linked-IPLD length does not fit this platform")]
    LengthOverflow,
    /// A declared field runs past the end of the payload.
    #[error("truncated linked-IPLD payload")]
    Truncated,
    /// Link CIDs must be byte-lexicographically sorted and unique.
    #[error("linked-IPLD CIDs are not strictly ordered")]
    NonCanonicalLinks,
    /// Bytes remain after the declared application body.
    #[error("trailing bytes after linked-IPLD body")]
    TrailingLinkedBytes,
    /// The CID cannot fit the generic key's native-identity bound.
    #[error(transparent)]
    ObjectKey(#[from] ObjectKeyError),
}

impl IpldCid {
    /// Build a CID for one complete payload and frozen codec.
    pub fn new(codec: u64, payload: &[u8]) -> Result<Self, IpldError> {
        Self::from_digest(codec, Digest::hash(payload))
    }

    /// Build a CID from an already verified payload digest.
    pub fn from_digest(codec: u64, digest: Digest) -> Result<Self, IpldError> {
        validate_codec(codec)?;
        let mut bytes = Vec::with_capacity(40);
        encode_uvarint(1, &mut bytes);
        encode_uvarint(codec, &mut bytes);
        encode_uvarint(BLAKE3_256_MULTIHASH, &mut bytes);
        encode_uvarint(32, &mut bytes);
        bytes.extend_from_slice(digest.as_bytes());
        Ok(Self {
            bytes: Bytes::from(bytes),
            codec,
            digest,
        })
    }

    /// Decode one exact canonical CID.
    pub fn decode(bytes: &[u8]) -> Result<Self, IpldError> {
        let mut cursor = 0usize;
        let version = decode_uvarint(bytes, &mut cursor)?;
        if version != 1 {
            return Err(IpldError::UnsupportedCidVersion(version));
        }
        let codec = decode_uvarint(bytes, &mut cursor)?;
        validate_codec(codec)?;
        let multihash = decode_uvarint(bytes, &mut cursor)?;
        if multihash != BLAKE3_256_MULTIHASH {
            return Err(IpldError::UnsupportedMultihash(multihash));
        }
        let digest_len = decode_uvarint(bytes, &mut cursor)?;
        if digest_len != 32 {
            return Err(IpldError::InvalidDigestLength(digest_len));
        }
        let end = cursor.checked_add(32).ok_or(IpldError::LengthOverflow)?;
        let digest_bytes = bytes.get(cursor..end).ok_or(IpldError::Truncated)?;
        if end != bytes.len() {
            return Err(IpldError::TrailingCidBytes);
        }
        Ok(Self {
            bytes: Bytes::copy_from_slice(bytes),
            codec,
            digest: Digest::try_from(digest_bytes)
                .expect("the exact digest width was checked above"),
        })
    }

    /// Exact binary CID bytes stored as an [`ObjectKey`] native identity.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Frozen multicodec carried by this CID.
    pub fn codec(&self) -> u64 {
        self.codec
    }

    /// BLAKE3-256 payload digest carried by this CID.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// Namespace-qualified repository key for this CID.
    pub fn object_key(&self) -> Result<ObjectKey, IpldError> {
        let namespace = match self.codec {
            RAW_CODEC => IPLD_RAW_NAMESPACE,
            CASITA_LINKED_CODEC => IPLD_LINKED_NAMESPACE,
            _ => return Err(IpldError::UnsupportedCodec(self.codec)),
        };
        Ok(ObjectKey::new(
            namespace.parse().expect("frozen namespace"),
            self.bytes.clone(),
        )?)
    }
}

/// Canonical linked-IPLD payload construction and inspection.
pub struct LinkedIpld;

impl LinkedIpld {
    /// Encode strictly ordered link CIDs followed by opaque application bytes.
    pub fn encode(links: &[IpldCid], body: &[u8]) -> Result<Vec<u8>, IpldError> {
        if links
            .windows(2)
            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
        {
            return Err(IpldError::NonCanonicalLinks);
        }
        let mut encoded = Vec::new();
        encoded.extend_from_slice(LINKED_MAGIC);
        encoded.extend_from_slice(&(links.len() as u64).to_le_bytes());
        for link in links {
            encoded.extend_from_slice(&(link.as_bytes().len() as u64).to_le_bytes());
            encoded.extend_from_slice(link.as_bytes());
        }
        encoded.extend_from_slice(&(body.len() as u64).to_le_bytes());
        encoded.extend_from_slice(body);
        Ok(encoded)
    }

    /// Decode and validate a complete linked payload, returning its links and
    /// borrowed application body.
    pub fn decode(payload: &[u8], max_links: usize) -> Result<(Vec<IpldCid>, &[u8]), IpldError> {
        let mut cursor = 0usize;
        if take(payload, &mut cursor, LINKED_MAGIC.len())? != LINKED_MAGIC {
            return Err(IpldError::InvalidLinkedMagic);
        }
        let count = read_u64(payload, &mut cursor)?;
        let count = usize::try_from(count).map_err(|_| IpldError::LengthOverflow)?;
        if count > max_links {
            return Err(IpldError::LengthOverflow);
        }
        let mut links = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let len = read_u64(payload, &mut cursor)?;
            let len = usize::try_from(len).map_err(|_| IpldError::LengthOverflow)?;
            let cid = IpldCid::decode(take(payload, &mut cursor, len)?)?;
            if links
                .last()
                .is_some_and(|previous: &IpldCid| previous.as_bytes() >= cid.as_bytes())
            {
                return Err(IpldError::NonCanonicalLinks);
            }
            links.push(cid);
        }
        let body_len = read_u64(payload, &mut cursor)?;
        let body_len = usize::try_from(body_len).map_err(|_| IpldError::LengthOverflow)?;
        let body = take(payload, &mut cursor, body_len)?;
        if cursor != payload.len() {
            return Err(IpldError::TrailingLinkedBytes);
        }
        Ok((links, body))
    }
}

/// Terminal raw CID verifier.
pub struct RawIpldFormat {
    namespace: NamespaceId,
}

impl Default for RawIpldFormat {
    fn default() -> Self {
        Self {
            namespace: IPLD_RAW_NAMESPACE.parse().expect("frozen namespace"),
        }
    }
}

/// Canonical linked CID verifier.
pub struct LinkedIpldFormat {
    namespace: NamespaceId,
}

impl Default for LinkedIpldFormat {
    fn default() -> Self {
        Self {
            namespace: IPLD_LINKED_NAMESPACE.parse().expect("frozen namespace"),
        }
    }
}

#[async_trait::async_trait]
impl ObjectFormat for RawIpldFormat {
    fn namespace(&self) -> &NamespaceId {
        &self.namespace
    }

    async fn verify(
        &self,
        mut context: VerificationContext<'_>,
        limits: &FormatLimits,
    ) -> Result<VerifiedObject, FormatError> {
        let cid = cid_for_context(&context, RAW_CODEC)?;
        let mut buffer = vec![0u8; limits.read_buffer_bytes.max(1)];
        loop {
            let read = context.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            if context.observed_size() > limits.max_payload_bytes {
                return Err(FormatError::PayloadLimit {
                    limit: limits.max_payload_bytes,
                });
            }
        }
        verify_cid_digest(&cid, context.observed_digest())?;
        context.finish(Vec::new())
    }
}

#[async_trait::async_trait]
impl ObjectFormat for LinkedIpldFormat {
    fn namespace(&self) -> &NamespaceId {
        &self.namespace
    }

    async fn verify(
        &self,
        mut context: VerificationContext<'_>,
        limits: &FormatLimits,
    ) -> Result<VerifiedObject, FormatError> {
        let cid = cid_for_context(&context, CASITA_LINKED_CODEC)?;
        let limit = limits.max_metadata_bytes.min(limits.max_payload_bytes);
        let bytes = context.read_to_end_bounded(limit).await?;
        verify_cid_digest(&cid, context.observed_digest())?;
        let (links, _) = LinkedIpld::decode(&bytes, limits.max_links_per_object)
            .map_err(|error| invalid_payload(&self.namespace, error))?;
        let links = links
            .into_iter()
            .map(|link| {
                link.object_key()
                    .map_err(|error| invalid_payload(&self.namespace, error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        context.finish(links)
    }
}

/// Built-in v0.2 formats for registry composition.
pub(crate) fn formats() -> [Arc<dyn ObjectFormat>; 2] {
    [
        Arc::new(RawIpldFormat::default()),
        Arc::new(LinkedIpldFormat::default()),
    ]
}

fn cid_for_context(
    context: &VerificationContext<'_>,
    expected_codec: u64,
) -> Result<IpldCid, FormatError> {
    let cid = IpldCid::decode(context.key().native_id())
        .map_err(|error| invalid_payload(context.key().namespace(), error))?;
    if cid.codec() != expected_codec {
        return Err(invalid_payload(
            context.key().namespace(),
            IpldError::CodecMismatch {
                expected: expected_codec,
                actual: cid.codec(),
            },
        ));
    }
    Ok(cid)
}

fn verify_cid_digest(cid: &IpldCid, actual: Digest) -> Result<(), FormatError> {
    if cid.digest() != actual {
        return Err(FormatError::InvalidPayload {
            namespace: match cid.codec() {
                RAW_CODEC => IPLD_RAW_NAMESPACE,
                CASITA_LINKED_CODEC => IPLD_LINKED_NAMESPACE,
                _ => "ipld.unknown.v1",
            }
            .parse()
            .expect("fixed namespace"),
            message: IpldError::DigestMismatch {
                expected: cid.digest(),
                actual,
            }
            .to_string(),
        });
    }
    Ok(())
}

fn invalid_payload(namespace: &NamespaceId, error: impl std::fmt::Display) -> FormatError {
    FormatError::InvalidPayload {
        namespace: namespace.clone(),
        message: error.to_string(),
    }
}

fn validate_codec(codec: u64) -> Result<(), IpldError> {
    match codec {
        RAW_CODEC | CASITA_LINKED_CODEC => Ok(()),
        _ => Err(IpldError::UnsupportedCodec(codec)),
    }
}

fn encode_uvarint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn decode_uvarint(input: &[u8], cursor: &mut usize) -> Result<u64, IpldError> {
    let start = *cursor;
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *input.get(*cursor).ok_or(IpldError::InvalidVarint)?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err(IpldError::InvalidVarint);
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            let mut canonical = Vec::new();
            encode_uvarint(value, &mut canonical);
            if input.get(start..*cursor) != Some(canonical.as_slice()) {
                return Err(IpldError::InvalidVarint);
            }
            return Ok(value);
        }
    }
    Err(IpldError::InvalidVarint)
}

fn read_u64(input: &[u8], cursor: &mut usize) -> Result<u64, IpldError> {
    let bytes = take(input, cursor, 8)?;
    Ok(u64::from_le_bytes(
        bytes.try_into().expect("the exact width was requested"),
    ))
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], IpldError> {
    let end = cursor.checked_add(len).ok_or(IpldError::LengthOverflow)?;
    let bytes = input.get(*cursor..end).ok_or(IpldError::Truncated)?;
    *cursor = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{FormatRegistry, PayloadReader};

    struct SliceReader(std::io::Cursor<Vec<u8>>);

    #[async_trait::async_trait]
    impl PayloadReader for SliceReader {
        async fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.0, buffer)
        }
    }

    #[test]
    fn raw_empty_cid_is_frozen() {
        let cid = IpldCid::new(RAW_CODEC, b"").unwrap();
        assert_eq!(
            data_encoding::HEXLOWER.encode(cid.as_bytes()),
            "01551e20af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(IpldCid::decode(cid.as_bytes()).unwrap(), cid);
    }

    #[tokio::test]
    async fn linked_payload_roundtrip_and_verification() {
        let raw = IpldCid::new(RAW_CODEC, b"leaf").unwrap();
        let payload = LinkedIpld::encode(std::slice::from_ref(&raw), b"body").unwrap();
        let cid = IpldCid::new(CASITA_LINKED_CODEC, &payload).unwrap();
        let key = cid.object_key().unwrap();
        let registry = FormatRegistry::builtin();
        let mut reader = SliceReader(std::io::Cursor::new(payload));
        let verified = registry
            .verify(&key, &mut reader, &FormatLimits::default())
            .await
            .unwrap();
        assert_eq!(verified.record().links(), &[raw.object_key().unwrap()]);
    }

    #[test]
    fn linked_payload_rejects_duplicate_or_unsorted_links() {
        let a = IpldCid::new(RAW_CODEC, b"a").unwrap();
        assert_eq!(
            LinkedIpld::encode(&[a.clone(), a], b"").unwrap_err(),
            IpldError::NonCanonicalLinks
        );
    }
}
