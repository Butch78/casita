//! Frozen transport-neutral v0.2 transfer messages.

#[cfg(any(feature = "ssh", test))]
use bytes::Bytes;

#[cfg(any(feature = "ssh", feature = "fuzzing", test))]
use crate::object::ObjectRecord;
#[cfg(any(feature = "ssh", test))]
use crate::object::RootName;
use crate::object::{ObjectKey, RepositoryRevision, RootRecord};
#[cfg(any(feature = "ssh", test))]
use crate::path::PathComponent;
use crate::sync::{
    DestinationRoot, MAX_TRANSFER_REQUEST_BYTES, MAX_TRANSFER_REQUEST_OBJECTS, ObjectRequest,
    RequestedStatus, TransferProgress, TransferRequest,
};
#[cfg(any(feature = "ssh", test))]
use crate::sync::{PathProof, PathProofEntry};
#[cfg(test)]
use crate::{BlobId, Digest};

const REQUEST_MAGIC: &[u8] = b"casita-transfer-request-v1\0";
const PROGRESS_MAGIC: &[u8] = b"casita-transfer-progress-v1\0";
#[cfg(any(feature = "ssh", feature = "fuzzing"))]
const RECORDS_MAGIC: &[u8] = b"casita-transfer-records-v1\0";
#[cfg(feature = "fuzzing")]
const PRESENCE_MAGIC: &[u8] = b"casita-transfer-presence-v1\0";
#[cfg(any(feature = "ssh", test))]
const PATH_PROOF_REQUEST_MAGIC: &[u8] = b"casita-path-proof-request-v1\0";
#[cfg(any(feature = "ssh", test))]
const PATH_PROOF_MAGIC: &[u8] = b"casita-path-proof-v1\0";

/// A receiver's exact answer for one advertised immutable record.
#[cfg(feature = "fuzzing")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordPresence {
    /// Advertised key.
    pub key: ObjectKey,
    /// Receiver has a structurally identical complete record.
    pub exact_record: bool,
    /// Receiver has the record's exact payload bytes.
    pub payload: bool,
}

/// Transfer message decoding failure.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransferWireError {
    /// Message type/version prefix is absent or wrong.
    #[error("invalid transfer message magic")]
    InvalidMagic,
    /// Input ended inside a field.
    #[error("truncated transfer message")]
    Truncated,
    /// A declared length cannot fit the address space.
    #[error("transfer field length overflows this platform")]
    LengthOverflow,
    /// A message, item count, or field exceeds its protocol bound.
    #[error("transfer field length {actual} exceeds limit {limit}")]
    LengthLimit {
        /// Observed or declared length.
        actual: usize,
        /// Frozen or caller-selected protocol limit.
        limit: usize,
    },
    /// A boolean byte was not zero or one.
    #[error("invalid transfer boolean byte {0}")]
    InvalidBoolean(u8),
    /// A requested-status tag is unknown.
    #[error("invalid requested-status tag {0}")]
    InvalidStatus(u8),
    /// Bytes remain after the one message.
    #[error("trailing bytes after transfer message")]
    TrailingBytes,
    /// An embedded generic logical encoding is invalid.
    #[error("invalid embedded logical value: {0}")]
    Logical(String),
}

/// Encode a batch of complete logical record advertisements.
#[cfg(any(feature = "ssh", feature = "fuzzing"))]
pub fn encode_records(records: &[ObjectRecord]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(RECORDS_MAGIC);
    put_u64(&mut output, records.len() as u64);
    for record in records {
        put_bytes(&mut output, &record.encode());
    }
    output
}

/// Decode a bounded batch of complete logical record advertisements.
#[cfg(any(feature = "ssh", feature = "fuzzing"))]
pub fn decode_records(
    encoded: &[u8],
    max_records: usize,
    max_message_bytes: usize,
) -> Result<Vec<ObjectRecord>, TransferWireError> {
    check_message_limit(encoded, max_message_bytes)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(RECORDS_MAGIC)?;
    let count = decoder.count(max_records)?;
    let mut records = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        records.push(
            ObjectRecord::decode(decoder.bytes(max_message_bytes)?)
                .map_err(|error| TransferWireError::Logical(error.to_string()))?,
        );
    }
    decoder.finish()?;
    Ok(records)
}

/// Encode one bounded atomic path-proof request.
#[cfg(any(feature = "ssh", test))]
pub(crate) fn encode_path_proof_request(root: &RootName, components: &[PathComponent]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(PATH_PROOF_REQUEST_MAGIC);
    put_bytes(&mut output, root.as_str().as_bytes());
    put_u64(&mut output, components.len() as u64);
    for component in components {
        put_bytes(&mut output, component.as_bytes());
    }
    output
}

/// Decode one bounded atomic path-proof request.
#[cfg(any(feature = "ssh", test))]
pub(crate) fn decode_path_proof_request(
    encoded: &[u8],
) -> Result<(RootName, Vec<PathComponent>), TransferWireError> {
    check_message_limit(encoded, MAX_TRANSFER_REQUEST_BYTES)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(PATH_PROOF_REQUEST_MAGIC)?;
    let root = std::str::from_utf8(decoder.bytes(MAX_TRANSFER_REQUEST_BYTES)?)
        .map_err(|error| TransferWireError::Logical(error.to_string()))?;
    let root =
        RootName::try_from(root).map_err(|error| TransferWireError::Logical(error.to_string()))?;
    let count = decoder.count(MAX_TRANSFER_REQUEST_OBJECTS)?;
    let mut components = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let component = PathComponent::try_from(Bytes::copy_from_slice(
            decoder.bytes(crate::path::MAX_NAME_LEN)?,
        ))
        .map_err(|error| TransferWireError::Logical(error.to_string()))?;
        components.push(component);
    }
    decoder.finish()?;
    Ok((root, components))
}

/// Encode an untrusted atomic path proof for transport.
#[cfg(any(feature = "ssh", test))]
pub(crate) fn encode_path_proof(proof: &PathProof) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(PATH_PROOF_MAGIC);
    output.extend_from_slice(proof.revision.as_bytes());
    put_bytes(&mut output, proof.root_name.as_str().as_bytes());
    put_bytes(&mut output, &proof.root.encode());
    put_u64(&mut output, proof.directories.len() as u64);
    for entry in &proof.directories {
        put_bytes(&mut output, &entry.record.encode());
        put_bytes(&mut output, &entry.payload);
    }
    output
}

/// Decode a bounded untrusted atomic path proof.
#[cfg(any(feature = "ssh", test))]
pub(crate) fn decode_path_proof(
    encoded: &[u8],
    max_directories: usize,
    max_message_bytes: usize,
) -> Result<PathProof, TransferWireError> {
    check_message_limit(encoded, max_message_bytes)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(PATH_PROOF_MAGIC)?;
    let revision: [u8; 32] = decoder
        .take(32)?
        .try_into()
        .expect("the exact revision width was requested");
    let root_name = std::str::from_utf8(decoder.bytes(max_message_bytes)?)
        .map_err(|error| TransferWireError::Logical(error.to_string()))?;
    let root_name = RootName::try_from(root_name)
        .map_err(|error| TransferWireError::Logical(error.to_string()))?;
    let root = decode_key(decoder.bytes(max_message_bytes)?)?;
    let count = decoder.count(max_directories)?;
    let mut directories = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let record = ObjectRecord::decode(decoder.bytes(max_message_bytes)?)
            .map_err(|error| TransferWireError::Logical(error.to_string()))?;
        let payload = decoder.bytes(max_message_bytes)?.to_vec();
        directories.push(PathProofEntry { record, payload });
    }
    decoder.finish()?;
    Ok(PathProof {
        revision: RepositoryRevision::from_bytes(revision),
        root_name,
        root,
        directories,
    })
}

/// Encode exact record and payload presence answers.
#[cfg(feature = "fuzzing")]
pub fn encode_presence(entries: &[RecordPresence]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(PRESENCE_MAGIC);
    put_u64(&mut output, entries.len() as u64);
    for entry in entries {
        put_bytes(&mut output, &entry.key.encode());
        output.push(u8::from(entry.exact_record));
        output.push(u8::from(entry.payload));
    }
    output
}

/// Decode bounded exact record and payload presence answers.
#[cfg(feature = "fuzzing")]
pub fn decode_presence(
    encoded: &[u8],
    max_entries: usize,
    max_message_bytes: usize,
) -> Result<Vec<RecordPresence>, TransferWireError> {
    check_message_limit(encoded, max_message_bytes)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(PRESENCE_MAGIC)?;
    let count = decoder.count(max_entries)?;
    let mut entries = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let key = decode_key(decoder.bytes(max_message_bytes)?)?;
        entries.push(RecordPresence {
            key,
            exact_record: decoder.boolean()?,
            payload: decoder.boolean()?,
        });
    }
    decoder.finish()?;
    Ok(entries)
}

pub(super) fn encode_request(request: &TransferRequest) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(REQUEST_MAGIC);
    put_u64(&mut output, request.objects.len() as u64);
    for object in &request.objects {
        put_bytes(&mut output, &object.key.encode());
        output.push(u8::from(object.recursive));
    }
    put_u64(&mut output, request.roots.len() as u64);
    for root in &request.roots {
        put_bytes(
            &mut output,
            &RootRecord::new(root.name.clone(), root.target.clone()).encode(),
        );
    }
    output
}

pub(super) fn decode_request(encoded: &[u8]) -> Result<TransferRequest, TransferWireError> {
    check_message_limit(encoded, MAX_TRANSFER_REQUEST_BYTES)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(REQUEST_MAGIC)?;
    let object_count = decoder.count(MAX_TRANSFER_REQUEST_OBJECTS)?;
    let mut objects = Vec::with_capacity(object_count.min(1024));
    for _ in 0..object_count {
        objects.push(ObjectRequest {
            key: decode_key(decoder.bytes(MAX_TRANSFER_REQUEST_BYTES)?)?,
            recursive: decoder.boolean()?,
        });
    }
    let root_count = decoder.count(MAX_TRANSFER_REQUEST_OBJECTS)?;
    let mut roots = Vec::with_capacity(root_count.min(1024));
    for _ in 0..root_count {
        let root = RootRecord::decode(decoder.bytes(MAX_TRANSFER_REQUEST_BYTES)?)
            .map_err(|error| TransferWireError::Logical(error.to_string()))?;
        roots.push(DestinationRoot {
            name: root.name().clone(),
            target: root.target().clone(),
        });
    }
    decoder.finish()?;
    Ok(TransferRequest { objects, roots })
}

pub(super) fn encode_progress(progress: &TransferProgress) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(PROGRESS_MAGIC);
    output.extend_from_slice(progress.destination_revision.as_bytes());
    for count in [
        progress.published_objects,
        progress.payloads_sent,
        progress.payloads_reused,
        progress.chunks_sent,
        progress.chunks_reused,
        progress.slice_copy_bytes,
        progress.slice_literal_bytes,
    ] {
        put_u64(&mut output, count);
    }
    put_u64(&mut output, progress.requested.len() as u64);
    for status in &progress.requested {
        let (tag, key) = match status {
            RequestedStatus::Missing(key) => (0, key),
            RequestedStatus::Incomplete(key) => (1, key),
            RequestedStatus::Invalid(key) => (2, key),
            RequestedStatus::Complete(key) => (3, key),
        };
        output.push(tag);
        put_bytes(&mut output, &key.encode());
    }
    output
}

pub(super) fn decode_progress(encoded: &[u8]) -> Result<TransferProgress, TransferWireError> {
    check_message_limit(encoded, MAX_TRANSFER_REQUEST_BYTES)?;
    let mut decoder = Decoder::new(encoded);
    decoder.magic(PROGRESS_MAGIC)?;
    let revision: [u8; 32] = decoder
        .take(32)?
        .try_into()
        .expect("the exact revision width was requested");
    let destination_revision = RepositoryRevision::from_bytes(revision);
    let published_objects = decoder.u64()?;
    let payloads_sent = decoder.u64()?;
    let payloads_reused = decoder.u64()?;
    let chunks_sent = decoder.u64()?;
    let chunks_reused = decoder.u64()?;
    let slice_copy_bytes = decoder.u64()?;
    let slice_literal_bytes = decoder.u64()?;
    let status_count = decoder.count(MAX_TRANSFER_REQUEST_OBJECTS)?;
    let mut requested = Vec::with_capacity(status_count.min(1024));
    for _ in 0..status_count {
        let tag = decoder.byte()?;
        let key = decode_key(decoder.bytes(MAX_TRANSFER_REQUEST_BYTES)?)?;
        requested.push(match tag {
            0 => RequestedStatus::Missing(key),
            1 => RequestedStatus::Incomplete(key),
            2 => RequestedStatus::Invalid(key),
            3 => RequestedStatus::Complete(key),
            tag => return Err(TransferWireError::InvalidStatus(tag)),
        });
    }
    decoder.finish()?;
    Ok(TransferProgress {
        destination_revision,
        published_objects,
        payloads_sent,
        payloads_reused,
        chunks_sent,
        chunks_reused,
        slice_copy_bytes,
        slice_literal_bytes,
        requested,
    })
}

fn decode_key(encoded: &[u8]) -> Result<ObjectKey, TransferWireError> {
    ObjectKey::decode(encoded).map_err(|error| TransferWireError::Logical(error.to_string()))
}

fn check_message_limit(encoded: &[u8], limit: usize) -> Result<(), TransferWireError> {
    if encoded.len() > limit {
        Err(TransferWireError::LengthLimit {
            actual: encoded.len(),
            limit,
        })
    } else {
        Ok(())
    }
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    put_u64(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

struct Decoder<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, cursor: 0 }
    }

    fn magic(&mut self, expected: &[u8]) -> Result<(), TransferWireError> {
        if self.take(expected.len())? != expected {
            return Err(TransferWireError::InvalidMagic);
        }
        Ok(())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], TransferWireError> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(TransferWireError::LengthOverflow)?;
        let bytes = self
            .input
            .get(self.cursor..end)
            .ok_or(TransferWireError::Truncated)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, TransferWireError> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self) -> Result<bool, TransferWireError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            byte => Err(TransferWireError::InvalidBoolean(byte)),
        }
    }

    fn u64(&mut self) -> Result<u64, TransferWireError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .expect("the exact integer width was requested"),
        ))
    }

    fn count(&mut self, limit: usize) -> Result<usize, TransferWireError> {
        let count = usize::try_from(self.u64()?).map_err(|_| TransferWireError::LengthOverflow)?;
        if count > limit {
            return Err(TransferWireError::LengthLimit {
                actual: count,
                limit,
            });
        }
        Ok(count)
    }

    fn bytes(&mut self, limit: usize) -> Result<&'a [u8], TransferWireError> {
        let len = self.count(limit)?;
        self.take(len)
    }

    fn finish(self) -> Result<(), TransferWireError> {
        if self.cursor == self.input.len() {
            Ok(())
        } else {
            Err(TransferWireError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirectoryId, RootName};

    #[test]
    fn request_and_progress_roundtrip_and_reject_trailing_bytes() {
        let key = ObjectKey::blob(BlobId::new(Digest::from([3; 32])));
        let request = TransferRequest {
            objects: vec![ObjectRequest {
                key: key.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: RootName::try_from("copies/main").unwrap(),
                target: key.clone(),
            }],
        };
        let encoded = request.encode();
        assert_eq!(TransferRequest::decode(&encoded).unwrap(), request);
        let mut trailing = encoded;
        trailing.push(0);
        assert!(matches!(
            TransferRequest::decode(&trailing),
            Err(TransferWireError::TrailingBytes)
        ));

        let progress = TransferProgress {
            destination_revision: RepositoryRevision::from_bytes([9; 32]),
            published_objects: 1,
            payloads_sent: 2,
            payloads_reused: 3,
            chunks_sent: 4,
            chunks_reused: 5,
            slice_copy_bytes: 6,
            slice_literal_bytes: 7,
            requested: vec![RequestedStatus::Complete(key)],
        };
        assert_eq!(
            TransferProgress::decode(&progress.encode()).unwrap(),
            progress
        );
    }

    #[test]
    fn path_proof_request_and_response_are_bounded_and_exact() {
        let root_name = RootName::try_from("releases/current").unwrap();
        let components = vec![
            PathComponent::try_from("lib").unwrap(),
            PathComponent::try_from("artifact").unwrap(),
        ];
        let request = encode_path_proof_request(&root_name, &components);
        assert_eq!(
            decode_path_proof_request(&request).unwrap(),
            (root_name, components)
        );

        let root = ObjectKey::directory(DirectoryId::new(Digest::from([7; 32])));
        let payload = b"canonical directory".to_vec();
        let record = ObjectRecord::new(
            root.clone(),
            BlobId::new(Digest::hash(&payload)),
            payload.len() as u64,
            Vec::new(),
        )
        .unwrap();
        let proof = PathProof {
            revision: RepositoryRevision::from_bytes([9; 32]),
            root_name: RootName::try_from("releases/current").unwrap(),
            root,
            directories: vec![PathProofEntry { record, payload }],
        };
        let mut encoded = encode_path_proof(&proof);
        assert_eq!(
            decode_path_proof(&encoded, 2, MAX_TRANSFER_REQUEST_BYTES).unwrap(),
            proof
        );
        encoded.push(0);
        assert!(matches!(
            decode_path_proof(&encoded, 2, MAX_TRANSFER_REQUEST_BYTES),
            Err(TransferWireError::TrailingBytes)
        ));
        assert!(matches!(
            decode_path_proof(&encode_path_proof(&proof), 0, MAX_TRANSFER_REQUEST_BYTES),
            Err(TransferWireError::LengthLimit { .. })
        ));
    }
}
