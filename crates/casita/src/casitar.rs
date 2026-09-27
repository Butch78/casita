//! Frozen framing for portable, closure-complete Casitar archives.
//!
//! The header and frame codecs are runtime-independent. Native builds also
//! provide bounded asynchronous [`CasitarReader`] and [`CasitarWriter`]
//! adapters. Repository traversal, namespace verification, and final root
//! mutation remain higher-level workflows.

use crate::{BlobId, Digest, ObjectKey, ObjectRecord};

#[cfg(feature = "native")]
mod export;
#[cfg(feature = "native")]
mod import;
#[cfg(feature = "native")]
mod stream;
#[cfg(feature = "native")]
pub use export::{
    CasitarExportError, CasitarExportFilePolicy, CasitarExportReport, CasitarExportTarget,
};
#[cfg(feature = "native")]
pub use import::{
    CasitarImportError, CasitarImportReport, CasitarRootConflictPolicy, CasitarRootMapping,
};
#[cfg(feature = "native")]
pub use stream::{
    CasitarReadFrame, CasitarReader, CasitarStats, CasitarStreamError, CasitarStreamLimits,
    CasitarWriter, DEFAULT_CASITAR_STREAM_BUFFER_BYTES, DEFAULT_MAX_CASITAR_STREAM_ITEMS,
};

/// Exact eight-byte version marker at the beginning of every v1 archive.
pub const CASITAR_MAGIC: &[u8; 8] = b"casitar1";
/// Largest canonical v1 header body.
pub const MAX_CASITAR_HEADER_BYTES: usize = 4 * 1024 * 1024;
/// Largest number of distinct roots declared by one v1 archive.
pub const MAX_CASITAR_ROOTS: usize = 4_096;
/// Largest encoded logical record accepted in one v1 record frame.
pub const MAX_CASITAR_RECORD_BYTES: usize = 256 * 1024 * 1024;

const TAG_END: u8 = 0;
const TAG_PAYLOAD: u8 = 1;
const TAG_RECORD: u8 = 2;
const PROLOGUE_BYTES: usize = CASITAR_MAGIC.len() + 8;
const PAYLOAD_FRAME_HEADER_BYTES: usize = 1 + 32 + 8;
const RECORD_FRAME_PREFIX_BYTES: usize = 1 + 8;

/// Canonical set of exact object roots declared by an archive.
///
/// Root names are deliberately absent. Names are destination-owned mutable
/// retention policy; an importer chooses them only after every declared
/// closure has been received and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasitarHeader {
    roots: Vec<ObjectKey>,
}

impl CasitarHeader {
    /// Build a canonical non-empty root set.
    ///
    /// Input order and duplicates do not affect the encoded header.
    pub fn new(mut roots: Vec<ObjectKey>) -> Result<Self, CasitarError> {
        roots.sort();
        roots.dedup();
        validate_root_count(roots.len())?;

        let body_len = encoded_header_body_len(&roots)?;
        check_limit("header bytes", body_len as u64, MAX_CASITAR_HEADER_BYTES)?;
        Ok(Self { roots })
    }

    /// Canonical, strictly ordered, duplicate-free root keys.
    pub fn roots(&self) -> &[ObjectKey] {
        &self.roots
    }

    /// Encode the complete v1 prologue and canonical header body.
    pub fn encode(&self) -> Vec<u8> {
        let body_len = encoded_header_body_len(&self.roots)
            .expect("a constructed Casitar header has a representable length");
        let mut encoded = Vec::with_capacity(PROLOGUE_BYTES + body_len);
        encoded.extend_from_slice(CASITAR_MAGIC);
        encoded.extend_from_slice(&(body_len as u64).to_le_bytes());
        encoded.extend_from_slice(&(self.roots.len() as u64).to_le_bytes());
        for root in &self.roots {
            put_bytes(&mut encoded, &root.encode());
        }
        encoded
    }

    /// Decode one header prefix, returning the header and bytes consumed.
    ///
    /// Bytes after the returned prefix are archive frames and are not treated
    /// as trailing header data.
    pub fn decode_prefix(encoded: &[u8]) -> Result<(Self, usize), CasitarError> {
        if encoded.len() < PROLOGUE_BYTES {
            return Err(CasitarError::Truncated);
        }
        if &encoded[..CASITAR_MAGIC.len()] != CASITAR_MAGIC {
            return Err(CasitarError::InvalidMagic);
        }

        let declared = u64::from_le_bytes(
            encoded[CASITAR_MAGIC.len()..PROLOGUE_BYTES]
                .try_into()
                .expect("the prologue contains an exact u64"),
        );
        check_limit("header bytes", declared, MAX_CASITAR_HEADER_BYTES)?;
        let body_len = usize::try_from(declared).map_err(|_| CasitarError::LengthOverflow)?;
        let end = PROLOGUE_BYTES
            .checked_add(body_len)
            .ok_or(CasitarError::LengthOverflow)?;
        if encoded.len() < end {
            return Err(CasitarError::Truncated);
        }

        let mut decoder = Decoder::new(&encoded[PROLOGUE_BYTES..end]);
        let count = decoder.u64()?;
        check_limit("root count", count, MAX_CASITAR_ROOTS)?;
        let count = usize::try_from(count).map_err(|_| CasitarError::LengthOverflow)?;
        if count == 0 {
            return Err(CasitarError::EmptyRoots);
        }

        let mut roots = Vec::with_capacity(count.min(1_024));
        for _ in 0..count {
            let root = ObjectKey::decode(decoder.bytes(MAX_OBJECT_KEY_BYTES)?)
                .map_err(|error| CasitarError::Logical(error.to_string()))?;
            if roots.last().is_some_and(|previous| previous >= &root) {
                return Err(CasitarError::NonCanonicalRoots);
            }
            roots.push(root);
        }
        decoder.finish()?;
        Ok((Self { roots }, end))
    }
}

/// Header of one frame in the v1 archive stream.
///
/// A payload header is followed by exactly `size` plaintext bytes. A record
/// header and the end marker have no body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasitarFrameHeader {
    /// Clean end marker. A complete archive has no bytes after this frame.
    End,
    /// One complete plaintext payload follows this header.
    Payload {
        /// Expected BLAKE3 identity of the following bytes.
        payload: BlobId,
        /// Exact number of following plaintext bytes.
        size: u64,
    },
    /// One canonical immutable logical record.
    Record(ObjectRecord),
}

impl CasitarFrameHeader {
    /// Encode this frame header. Payload body bytes are not included.
    pub fn encode(&self) -> Result<Vec<u8>, CasitarError> {
        match self {
            Self::End => Ok(vec![TAG_END]),
            Self::Payload { payload, size } => {
                let mut encoded = Vec::with_capacity(PAYLOAD_FRAME_HEADER_BYTES);
                encoded.push(TAG_PAYLOAD);
                encoded.extend_from_slice(payload.digest().as_bytes());
                encoded.extend_from_slice(&size.to_le_bytes());
                Ok(encoded)
            }
            Self::Record(record) => {
                let record = record.encode();
                check_limit(
                    "record bytes",
                    record.len() as u64,
                    MAX_CASITAR_RECORD_BYTES,
                )?;
                let mut encoded = Vec::with_capacity(RECORD_FRAME_PREFIX_BYTES + record.len());
                encoded.push(TAG_RECORD);
                encoded.extend_from_slice(&(record.len() as u64).to_le_bytes());
                encoded.extend_from_slice(&record);
                Ok(encoded)
            }
        }
    }

    /// Decode one frame-header prefix and return the number of bytes consumed.
    ///
    /// For [`Payload`](Self::Payload), the returned count stops before the
    /// plaintext body. The caller must read or skip exactly the declared size
    /// before decoding the next frame.
    pub fn decode_prefix(encoded: &[u8]) -> Result<(Self, usize), CasitarError> {
        let Some(&tag) = encoded.first() else {
            return Err(CasitarError::Truncated);
        };
        match tag {
            TAG_END => Ok((Self::End, 1)),
            TAG_PAYLOAD => {
                if encoded.len() < PAYLOAD_FRAME_HEADER_BYTES {
                    return Err(CasitarError::Truncated);
                }
                let payload = BlobId::new(
                    Digest::try_from(&encoded[1..33])
                        .expect("the payload frame requests exactly one digest"),
                );
                let size = u64::from_le_bytes(
                    encoded[33..41]
                        .try_into()
                        .expect("the payload frame requests exactly one u64"),
                );
                Ok((Self::Payload { payload, size }, PAYLOAD_FRAME_HEADER_BYTES))
            }
            TAG_RECORD => {
                if encoded.len() < RECORD_FRAME_PREFIX_BYTES {
                    return Err(CasitarError::Truncated);
                }
                let declared = u64::from_le_bytes(
                    encoded[1..9]
                        .try_into()
                        .expect("the record frame requests exactly one u64"),
                );
                check_limit("record bytes", declared, MAX_CASITAR_RECORD_BYTES)?;
                let record_len =
                    usize::try_from(declared).map_err(|_| CasitarError::LengthOverflow)?;
                let end = RECORD_FRAME_PREFIX_BYTES
                    .checked_add(record_len)
                    .ok_or(CasitarError::LengthOverflow)?;
                if encoded.len() < end {
                    return Err(CasitarError::Truncated);
                }
                let record = ObjectRecord::decode(&encoded[RECORD_FRAME_PREFIX_BYTES..end])
                    .map_err(|error| CasitarError::Logical(error.to_string()))?;
                Ok((Self::Record(record), end))
            }
            other => Err(CasitarError::InvalidFrameTag(other)),
        }
    }
}

/// Invalid or noncanonical Casitar framing.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CasitarError {
    /// The v1 marker is absent or wrong.
    #[error("invalid Casitar magic")]
    InvalidMagic,
    /// Input ended inside a declared field or frame header.
    #[error("truncated Casitar encoding")]
    Truncated,
    /// A decoded length cannot fit the current address space.
    #[error("Casitar length overflows this platform")]
    LengthOverflow,
    /// A container field exceeds the frozen v1 ceiling.
    #[error("Casitar {field} {actual} exceeds limit {limit}")]
    LengthLimit {
        /// Name of the bounded field.
        field: &'static str,
        /// Untrusted or computed value.
        actual: u64,
        /// Frozen v1 ceiling.
        limit: u64,
    },
    /// An archive must declare at least one root.
    #[error("Casitar root set is empty")]
    EmptyRoots,
    /// Root keys were duplicated or not strictly ordered.
    #[error("Casitar roots are not in canonical strictly ascending order")]
    NonCanonicalRoots,
    /// A frame tag has no v1 meaning.
    #[error("invalid Casitar frame tag {0}")]
    InvalidFrameTag(u8),
    /// A root key or object record did not use its frozen canonical encoding.
    #[error("invalid logical value in Casitar: {0}")]
    Logical(String),
    /// A length-delimited header body contains unconsumed bytes.
    #[error("trailing bytes in Casitar header")]
    TrailingHeaderBytes,
}

const MAX_OBJECT_KEY_BYTES: usize =
    8 + crate::object::MAX_NAMESPACE_LEN + 8 + crate::object::MAX_NATIVE_ID_LEN;

fn validate_root_count(count: usize) -> Result<(), CasitarError> {
    if count == 0 {
        return Err(CasitarError::EmptyRoots);
    }
    check_limit("root count", count as u64, MAX_CASITAR_ROOTS)
}

fn encoded_header_body_len(roots: &[ObjectKey]) -> Result<usize, CasitarError> {
    roots.iter().try_fold(8usize, |total, root| {
        total
            .checked_add(8)
            .and_then(|value| value.checked_add(root.encode().len()))
            .ok_or(CasitarError::LengthOverflow)
    })
}

fn check_limit(field: &'static str, actual: u64, limit: usize) -> Result<(), CasitarError> {
    if actual > limit as u64 {
        Err(CasitarError::LengthLimit {
            field,
            actual,
            limit: limit as u64,
        })
    } else {
        Ok(())
    }
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

struct Decoder<'a> {
    encoded: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    fn new(encoded: &'a [u8]) -> Self {
        Self {
            encoded,
            position: 0,
        }
    }

    fn read(&mut self, length: usize) -> Result<&'a [u8], CasitarError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CasitarError::LengthOverflow)?;
        if end > self.encoded.len() {
            return Err(CasitarError::Truncated);
        }
        let value = &self.encoded[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn u64(&mut self) -> Result<u64, CasitarError> {
        Ok(u64::from_le_bytes(
            self.read(8)?
                .try_into()
                .expect("eight bytes were requested"),
        ))
    }

    fn bytes(&mut self, limit: usize) -> Result<&'a [u8], CasitarError> {
        let length = self.u64()?;
        check_limit("embedded value bytes", length, limit)?;
        let length = usize::try_from(length).map_err(|_| CasitarError::LengthOverflow)?;
        self.read(length)
    }

    fn finish(self) -> Result<(), CasitarError> {
        if self.position == self.encoded.len() {
            Ok(())
        } else {
            Err(CasitarError::TrailingHeaderBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> ObjectKey {
        ObjectKey::blob(BlobId::new(Digest::from([byte; 32])))
    }

    #[test]
    fn header_construction_is_a_canonical_set() {
        let header = CasitarHeader::new(vec![key(2), key(1), key(2)]).unwrap();
        assert_eq!(header.roots(), &[key(1), key(2)]);
        let encoded = header.encode();
        let (decoded, consumed) = CasitarHeader::decode_prefix(&encoded).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn header_rejects_empty_noncanonical_and_trailing_bodies() {
        assert_eq!(
            CasitarHeader::new(Vec::new()),
            Err(CasitarError::EmptyRoots)
        );

        let canonical = CasitarHeader::new(vec![key(1), key(2)]).unwrap().encode();
        let body_start = PROLOGUE_BYTES;
        let first_len = 8 + key(1).encode().len();
        let mut descending = canonical.clone();
        let first = canonical[body_start + 8..body_start + 8 + first_len].to_vec();
        let second = canonical[body_start + 8 + first_len..].to_vec();
        descending[body_start + 8..body_start + 8 + second.len()].copy_from_slice(&second);
        descending[body_start + 8 + second.len()..].copy_from_slice(&first);
        assert_eq!(
            CasitarHeader::decode_prefix(&descending),
            Err(CasitarError::NonCanonicalRoots)
        );

        let mut trailing = CasitarHeader::new(vec![key(1)]).unwrap().encode();
        let body_len = u64::from_le_bytes(trailing[8..16].try_into().unwrap());
        trailing[8..16].copy_from_slice(&(body_len + 1).to_le_bytes());
        trailing.push(0xff);
        assert_eq!(
            CasitarHeader::decode_prefix(&trailing),
            Err(CasitarError::TrailingHeaderBytes)
        );
    }

    #[test]
    fn every_truncated_header_prefix_is_rejected() {
        let encoded = CasitarHeader::new(vec![key(1)]).unwrap().encode();
        for length in 0..encoded.len() {
            assert_eq!(
                CasitarHeader::decode_prefix(&encoded[..length]),
                Err(CasitarError::Truncated),
                "accepted prefix length {length}"
            );
        }
    }

    #[test]
    fn frame_headers_roundtrip_and_leave_payload_bytes_unconsumed() {
        let payload = BlobId::new(Digest::from([3; 32]));
        let payload_header = CasitarFrameHeader::Payload { payload, size: 5 };
        let mut encoded = payload_header.encode().unwrap();
        encoded.extend_from_slice(b"hello");
        let (decoded, consumed) = CasitarFrameHeader::decode_prefix(&encoded).unwrap();
        assert_eq!(decoded, payload_header);
        assert_eq!(consumed, PAYLOAD_FRAME_HEADER_BYTES);
        assert_eq!(&encoded[consumed..], b"hello");

        let record = ObjectRecord::new(key(4), payload, 5, vec![key(5)]).unwrap();
        let record_header = CasitarFrameHeader::Record(record);
        let encoded = record_header.encode().unwrap();
        assert_eq!(
            CasitarFrameHeader::decode_prefix(&encoded).unwrap(),
            (record_header, encoded.len())
        );
        assert_eq!(
            CasitarFrameHeader::decode_prefix(&[TAG_END]).unwrap(),
            (CasitarFrameHeader::End, 1)
        );
    }

    #[test]
    fn frame_headers_reject_truncation_unknown_tags_and_oversized_records() {
        let payload = CasitarFrameHeader::Payload {
            payload: BlobId::new(Digest::from([6; 32])),
            size: 9,
        }
        .encode()
        .unwrap();
        for length in 0..payload.len() {
            assert_eq!(
                CasitarFrameHeader::decode_prefix(&payload[..length]),
                Err(CasitarError::Truncated)
            );
        }
        assert_eq!(
            CasitarFrameHeader::decode_prefix(&[0xff]),
            Err(CasitarError::InvalidFrameTag(0xff))
        );

        let mut oversized = vec![TAG_RECORD];
        oversized.extend_from_slice(&((MAX_CASITAR_RECORD_BYTES as u64) + 1).to_le_bytes());
        assert!(matches!(
            CasitarFrameHeader::decode_prefix(&oversized),
            Err(CasitarError::LengthLimit {
                field: "record bytes",
                ..
            })
        ));
    }
}
