//! Canonical application metadata with generic forward links.
//!
//! A linked object is the smallest format an application needs to make its
//! own metadata part of Casita's verified closure graph. Casita treats the
//! body as opaque bytes, while the canonical link set participates in
//! transfer, retention, verification, and garbage collection.

use crate::format::{FormatError, FormatLimits, ObjectFormat, VerificationContext, VerifiedObject};
use crate::object::{NamespaceId, ObjectKey};
use crate::{Digest, ObjectKeyError};

/// Frozen namespace for generic linked application metadata.
pub const LINKED_OBJECT_NAMESPACE: &str = "casita.linked.v1";

const LINKED_MAGIC: &[u8] = b"casita-linked-v1\0";

/// Why a canonical linked-object payload is invalid.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkedObjectError {
    /// The fixed format marker is absent or incorrect.
    #[error("invalid linked-object magic")]
    InvalidMagic,
    /// A declared count or length cannot fit the current address space.
    #[error("linked-object length does not fit this platform")]
    LengthOverflow,
    /// A declared field extends beyond the payload.
    #[error("truncated linked-object payload")]
    Truncated,
    /// Links must be strictly ordered and unique by canonical object key.
    #[error("linked-object links are not strictly ordered")]
    NonCanonicalLinks,
    /// Bytes remain after the declared body.
    #[error("trailing bytes after linked-object body")]
    TrailingBytes,
    /// One embedded object key is malformed.
    #[error("invalid linked-object key: {0}")]
    InvalidObjectKey(String),
    /// The native identifier is not one BLAKE3 digest.
    #[error("linked-object native identifier must be 32 bytes, got {0}")]
    InvalidNativeId(usize),
    /// Complete payload bytes disagree with the native identifier.
    #[error("linked-object digest mismatch: expected {expected}, observed {actual}")]
    DigestMismatch {
        /// Digest carried by the object key.
        expected: Digest,
        /// Digest computed from the complete canonical payload.
        actual: Digest,
    },
    /// The generated key exceeds generic object-key limits.
    #[error(transparent)]
    ObjectKey(#[from] ObjectKeyError),
}

/// Canonical generic link-set plus opaque application body.
pub struct LinkedObject;

impl LinkedObject {
    /// Encode an ordered, duplicate-free set of generic object links.
    pub fn encode(links: &[ObjectKey], body: &[u8]) -> Result<Vec<u8>, LinkedObjectError> {
        if links.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(LinkedObjectError::NonCanonicalLinks);
        }
        let mut encoded = Vec::new();
        encoded.extend_from_slice(LINKED_MAGIC);
        encoded.extend_from_slice(&(links.len() as u64).to_le_bytes());
        for link in links {
            let key = link.encode();
            encoded.extend_from_slice(&(key.len() as u64).to_le_bytes());
            encoded.extend_from_slice(&key);
        }
        encoded.extend_from_slice(&(body.len() as u64).to_le_bytes());
        encoded.extend_from_slice(body);
        Ok(encoded)
    }

    /// Decode one canonical payload into its links and borrowed body.
    pub fn decode(
        payload: &[u8],
        max_links: usize,
    ) -> Result<(Vec<ObjectKey>, &[u8]), LinkedObjectError> {
        let mut cursor = 0usize;
        if take(payload, &mut cursor, LINKED_MAGIC.len())? != LINKED_MAGIC {
            return Err(LinkedObjectError::InvalidMagic);
        }
        let count = usize::try_from(read_u64(payload, &mut cursor)?)
            .map_err(|_| LinkedObjectError::LengthOverflow)?;
        if count > max_links {
            return Err(LinkedObjectError::LengthOverflow);
        }
        let mut links = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let length = usize::try_from(read_u64(payload, &mut cursor)?)
                .map_err(|_| LinkedObjectError::LengthOverflow)?;
            let encoded = take(payload, &mut cursor, length)?;
            let key = ObjectKey::decode(encoded)
                .map_err(|error| LinkedObjectError::InvalidObjectKey(error.to_string()))?;
            if links.last().is_some_and(|previous| previous >= &key) {
                return Err(LinkedObjectError::NonCanonicalLinks);
            }
            links.push(key);
        }
        let body_length = usize::try_from(read_u64(payload, &mut cursor)?)
            .map_err(|_| LinkedObjectError::LengthOverflow)?;
        let body = take(payload, &mut cursor, body_length)?;
        if cursor != payload.len() {
            return Err(LinkedObjectError::TrailingBytes);
        }
        Ok((links, body))
    }

    /// Construct the namespace-qualified identity of a canonical payload.
    pub fn key(payload: &[u8]) -> Result<ObjectKey, LinkedObjectError> {
        Self::key_in(
            &LINKED_OBJECT_NAMESPACE.parse().expect("frozen namespace"),
            payload,
        )
    }

    /// Construct the identity of a canonical payload in an application-owned
    /// namespace that uses linked-object framing.
    pub fn key_in(namespace: &NamespaceId, payload: &[u8]) -> Result<ObjectKey, LinkedObjectError> {
        Ok(ObjectKey::new(
            namespace.clone(),
            Digest::hash(payload).as_bytes().to_vec(),
        )?)
    }
}

/// Verifier for [`LINKED_OBJECT_NAMESPACE`].
pub struct LinkedObjectFormat {
    namespace: NamespaceId,
}

impl Default for LinkedObjectFormat {
    fn default() -> Self {
        Self {
            namespace: LINKED_OBJECT_NAMESPACE.parse().expect("frozen namespace"),
        }
    }
}

impl LinkedObjectFormat {
    /// Verify linked-object framing under an application-owned namespace.
    pub fn new(namespace: NamespaceId) -> Self {
        Self { namespace }
    }
}

#[async_trait::async_trait]
impl ObjectFormat for LinkedObjectFormat {
    fn namespace(&self) -> &NamespaceId {
        &self.namespace
    }

    async fn verify(
        &self,
        mut context: VerificationContext<'_>,
        limits: &FormatLimits,
    ) -> Result<VerifiedObject, FormatError> {
        let native = context.key().native_id();
        let expected = Digest::try_from(native).map_err(|_| FormatError::InvalidPayload {
            namespace: self.namespace.clone(),
            message: LinkedObjectError::InvalidNativeId(native.len()).to_string(),
        })?;
        let bytes = context
            .read_to_end_bounded(limits.max_metadata_bytes.min(limits.max_payload_bytes))
            .await?;
        let actual = context.observed_digest();
        if actual != expected {
            return Err(FormatError::InvalidPayload {
                namespace: self.namespace.clone(),
                message: LinkedObjectError::DigestMismatch { expected, actual }.to_string(),
            });
        }
        let (links, _) =
            LinkedObject::decode(&bytes, limits.max_links_per_object).map_err(|error| {
                FormatError::InvalidPayload {
                    namespace: self.namespace.clone(),
                    message: error.to_string(),
                }
            })?;
        context.finish(links)
    }
}

fn read_u64(payload: &[u8], cursor: &mut usize) -> Result<u64, LinkedObjectError> {
    let bytes: [u8; 8] = take(payload, cursor, 8)?
        .try_into()
        .expect("take returned the requested width");
    Ok(u64::from_le_bytes(bytes))
}

fn take<'a>(
    payload: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], LinkedObjectError> {
    let end = cursor
        .checked_add(length)
        .ok_or(LinkedObjectError::LengthOverflow)?;
    let bytes = payload
        .get(*cursor..end)
        .ok_or(LinkedObjectError::Truncated)?;
    *cursor = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "native")]
    use crate::{
        BlobFormat, BlobId, DestinationRoot, DirectoryFormat, DirectoryId, FormatRegistry,
        MemoryBlobStore, MemoryMetadataStore, ObjectRequest, RootName, TransferRequest,
        repository::Repository, transfer,
    };
    #[cfg(not(feature = "native"))]
    use crate::{BlobId, DirectoryId};
    #[cfg(feature = "native")]
    use std::sync::Arc;

    #[test]
    fn round_trip_preserves_cross_namespace_links_and_body() {
        let mut links = vec![
            ObjectKey::blob(BlobId::new(Digest::hash(b"blob"))),
            ObjectKey::directory(DirectoryId::new(Digest::hash(b"directory"))),
        ];
        links.sort();
        let payload = LinkedObject::encode(&links, b"application metadata").unwrap();
        let (decoded, body) = LinkedObject::decode(&payload, links.len()).unwrap();
        assert_eq!(decoded, links);
        assert_eq!(body, b"application metadata");
        assert_eq!(
            LinkedObject::key(&payload).unwrap().namespace().as_str(),
            LINKED_OBJECT_NAMESPACE
        );
    }

    #[test]
    fn rejects_noncanonical_links_and_trailing_bytes() {
        let key = ObjectKey::blob(BlobId::new(Digest::hash(b"duplicate")));
        assert_eq!(
            LinkedObject::encode(&[key.clone(), key], b"body").unwrap_err(),
            LinkedObjectError::NonCanonicalLinks
        );

        let mut payload = LinkedObject::encode(&[], b"body").unwrap();
        payload.push(0);
        assert_eq!(
            LinkedObject::decode(&payload, 0).unwrap_err(),
            LinkedObjectError::TrailingBytes
        );
    }

    #[test]
    fn application_namespace_owns_the_framed_object_identity() {
        let namespace: NamespaceId = "example.release.v1".parse().unwrap();
        let payload = LinkedObject::encode(&[], b"release metadata").unwrap();
        let key = LinkedObject::key_in(&namespace, &payload).unwrap();
        assert_eq!(key.namespace(), &namespace);
        assert_eq!(
            LinkedObjectFormat::new(namespace).namespace().as_str(),
            "example.release.v1"
        );
    }

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn registry_verifies_the_declared_forward_closure() {
        let leaf = ObjectKey::blob(BlobId::new(Digest::hash(b"leaf")));
        let payload = LinkedObject::encode(std::slice::from_ref(&leaf), b"root").unwrap();
        let key = LinkedObject::key(&payload).unwrap();
        let mut reader = std::io::Cursor::new(payload);
        let verified = FormatRegistry::builtin()
            .verify(&key, &mut reader, &FormatLimits::default())
            .await
            .unwrap();
        assert_eq!(verified.record().links(), [leaf]);
    }

    #[cfg(feature = "native")]
    #[tokio::test]
    async fn recursive_transfer_copies_and_names_a_mixed_format_closure() {
        let namespace: NamespaceId = "example.release.v1".parse().unwrap();
        let formats = FormatRegistry::new([
            Arc::new(BlobFormat::default()) as Arc<dyn ObjectFormat>,
            Arc::new(DirectoryFormat::default()) as Arc<dyn ObjectFormat>,
            Arc::new(LinkedObjectFormat::new(namespace.clone())) as Arc<dyn ObjectFormat>,
        ])
        .unwrap();
        let source = Repository::with_formats(
            MemoryBlobStore::new(),
            MemoryMetadataStore::new().unwrap(),
            formats.clone(),
            FormatLimits::default(),
        );
        let mutation = source.mutation_session().await.unwrap();
        let leaf = mutation.stage_blob(b"leaf").await.unwrap();
        let leaf_key = leaf.record().key().clone();
        let payload = LinkedObject::encode(std::slice::from_ref(&leaf_key), b"metadata").unwrap();
        let root_key = LinkedObject::key_in(&namespace, &payload).unwrap();
        let root = mutation
            .stage_object(root_key.clone(), &payload)
            .await
            .unwrap();
        mutation.publish_unrooted(vec![leaf, root]).await.unwrap();

        let destination = Repository::with_formats(
            MemoryBlobStore::new(),
            MemoryMetadataStore::new().unwrap(),
            formats,
            FormatLimits::default(),
        );
        let name = RootName::try_from("applications/release").unwrap();
        let result = transfer(
            &source,
            &destination,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: root_key.clone(),
                    recursive: true,
                }],
                roots: vec![DestinationRoot {
                    name: name.clone(),
                    target: root_key.clone(),
                }],
            },
            crate::sync::TransferOptions::default(),
        )
        .await
        .unwrap();

        assert_eq!(result.progress.published_objects, 2);
        let hold = destination.retention_hold().await.unwrap();
        assert_eq!(
            hold.snapshot().root(&name).await.unwrap(),
            Some(root_key.clone())
        );
        assert!(matches!(
            hold.verify_closure(&root_key).await.unwrap(),
            crate::ClosureStatus::Complete { objects: 2 }
        ));
    }
}
