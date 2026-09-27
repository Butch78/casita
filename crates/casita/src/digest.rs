//! The raw BLAKE3 [`Digest`] primitive and the typed content IDs built on it.

use std::fmt;
use std::str::FromStr;

use data_encoding::{BASE64URL_NOPAD, HEXLOWER};

/// Length of a BLAKE3 digest, in bytes.
pub const DIGEST_LEN: usize = blake3::OUT_LEN;

/// A raw BLAKE3 digest underlying every typed content address.
///
/// Most object APIs take [`BlobId`] or [`DirectoryId`] instead,
/// so a caller must select the intended capability
/// explicitly. This primitive displays and parses as `blake3-<base64url>`,
/// using the unpadded URL-safe
/// alphabet so the textual form is safe in URLs, filenames, and shells. The
/// raw 32 bytes are also available as lowercase hex via [`Digest::to_hex`]
/// (used for sharded on-disk paths).
///
/// Ordering is byte-lexicographic over the raw digest, so digests can key a
/// `BTreeMap`/`BTreeSet` or be sorted for deterministic output. Content
/// addresses are public, so the comparison is not constant-time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Digest([u8; DIGEST_LEN]);

/// Errors constructing or parsing a [`Digest`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DigestError {
    /// The byte slice was not exactly 32 bytes long.
    #[error("invalid digest length: {0} (expected {DIGEST_LEN})")]
    InvalidLength(usize),
    /// The string did not begin with the `blake3-` prefix.
    #[error("invalid hash type (expected `blake3-` prefix)")]
    InvalidHashType,
    /// The base64 body failed to decode.
    #[error("invalid base64: {0}")]
    InvalidBase64(String),
    /// A lowercase-hex encoding failed to decode.
    #[error("invalid hex: {0}")]
    InvalidHex(String),
}

impl Digest {
    /// The BLAKE3 digest of `data`: the content address casita gives a blob
    /// with exactly these bytes. For incremental hashing, feed a
    /// [`blake3::Hasher`] (blake3 is re-exported at the crate root) and
    /// convert the result with `Digest::from`.
    pub fn hash(data: &[u8]) -> Self {
        blake3::hash(data).into()
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// Lowercase-hex encoding of the digest (used for on-disk sharded paths).
    pub fn to_hex(&self) -> String {
        HEXLOWER.encode(&self.0)
    }

    /// Parse a digest from its lowercase-hex encoding (the inverse of
    /// [`Digest::to_hex`]).
    pub fn from_hex(s: &str) -> Result<Self, DigestError> {
        let bytes = HEXLOWER
            .decode(s.as_bytes())
            .map_err(|e| DigestError::InvalidHex(e.to_string()))?;
        Self::try_from(bytes)
    }
}

impl From<[u8; DIGEST_LEN]> for Digest {
    fn from(v: [u8; DIGEST_LEN]) -> Self {
        Self(v)
    }
}

impl From<blake3::Hash> for Digest {
    fn from(h: blake3::Hash) -> Self {
        Self(*h.as_bytes())
    }
}

impl TryFrom<&[u8]> for Digest {
    type Error = DigestError;

    fn try_from(v: &[u8]) -> Result<Self, Self::Error> {
        let arr: [u8; DIGEST_LEN] = v
            .try_into()
            .map_err(|_| DigestError::InvalidLength(v.len()))?;
        Ok(Self(arr))
    }
}

impl TryFrom<Vec<u8>> for Digest {
    type Error = DigestError;

    fn try_from(v: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_from(v.as_slice())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3-{}", BASE64URL_NOPAD.encode(&self.0))
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s
            .strip_prefix("blake3-")
            .ok_or(DigestError::InvalidHashType)?;
        let bytes = BASE64URL_NOPAD
            .decode(body.as_bytes())
            .map_err(|e| DigestError::InvalidBase64(e.to_string()))?;
        Self::try_from(bytes)
    }
}

macro_rules! typed_id {
    ($name:ident, $what:literal) => {
        #[doc = concat!("The typed content identifier of ", $what, ".")]
        ///
        /// This is representationally identical to [`Digest`], but is a
        /// distinct Rust type so an identifier cannot be used at the wrong
        /// object boundary accidentally.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub struct $name(Digest);

        impl $name {
            /// Label an already-computed digest with this semantic kind.
            ///
            /// Construction is deliberately explicit: a digest alone cannot
            /// prove what kind of object is stored under it.
            pub const fn new(digest: Digest) -> Self {
                Self(digest)
            }

            /// The underlying cryptographic digest.
            pub const fn digest(self) -> Digest {
                self.0
            }

            /// Borrow the underlying cryptographic digest.
            pub const fn as_digest(&self) -> &Digest {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl FromStr for $name {
            type Err = DigestError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse().map(Self::new)
            }
        }
    };
}

typed_id!(BlobId, "a blob");
typed_id!(DirectoryId, "a directory");
typed_id!(ChunkId, "a blob chunk");
#[cfg(feature = "native")]
typed_id!(PackId, "an immutable chunk pack");

/// A typed identifier for either user-visible object kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectId {
    /// Raw file contents.
    Blob(BlobId),
    /// A canonical directory encoding.
    Directory(DirectoryId),
}

impl ObjectId {
    /// The underlying cryptographic digest.
    pub const fn digest(self) -> Digest {
        match self {
            Self::Blob(id) => id.digest(),
            Self::Directory(id) => id.digest(),
        }
    }
}

impl From<BlobId> for ObjectId {
    fn from(id: BlobId) -> Self {
        Self::Blob(id)
    }
}

impl From<DirectoryId> for ObjectId {
    fn from(id: DirectoryId) -> Self {
        Self::Directory(id)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blob(id) => write!(f, "blob {id}"),
            Self::Directory(id) => write!(f, "directory {id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_fromstr_roundtrip() {
        let d: Digest = blake3::hash(b"hello").into();
        let s = d.to_string();
        assert!(s.starts_with("blake3-"), "{s}");
        assert_eq!(d, s.parse().unwrap());
        // the textual form must stay safe in URLs, filenames, and shells:
        // URL-safe alphabet, no padding.
        assert!(
            !s.contains('+') && !s.contains('/') && !s.contains('='),
            "{s}"
        );
    }

    #[test]
    fn hash_matches_blake3() {
        assert_eq!(Digest::hash(b"hello"), blake3::hash(b"hello").into());
    }

    #[test]
    fn hex_roundtrip() {
        let d: Digest = blake3::hash(b"hello").into();
        assert_eq!(d.to_hex().len(), 64);
        assert_eq!(Digest::from_hex(&d.to_hex()).unwrap(), d);
    }

    #[test]
    fn ordering_is_byte_lexicographic() {
        let lo: Digest = [0u8; DIGEST_LEN].into();
        let mut mid_bytes = [0u8; DIGEST_LEN];
        mid_bytes[0] = 1;
        let mid: Digest = mid_bytes.into();
        let hi: Digest = [0xffu8; DIGEST_LEN].into();
        assert!(lo < mid && mid < hi);
        // usable as a sort key and a BTreeSet element.
        let mut sorted = [hi, lo, mid];
        sorted.sort();
        assert_eq!(sorted, [lo, mid, hi]);
        let set = std::collections::BTreeSet::from([hi, lo, mid]);
        assert_eq!(set.iter().next(), Some(&lo));
    }

    #[test]
    fn wrong_length_rejected() {
        assert_eq!(
            Digest::try_from([0u8; 10].as_slice()).unwrap_err(),
            DigestError::InvalidLength(10)
        );
    }

    #[test]
    fn fromstr_requires_prefix() {
        assert_eq!(
            "deadbeef".parse::<Digest>().unwrap_err(),
            DigestError::InvalidHashType
        );
    }

    #[test]
    fn empty_blob_digest_is_stable() {
        // BLAKE3 of the empty input is a well-known constant; the empty blob's
        // address relies on it being stable.
        let d: Digest = blake3::hash(b"").into();
        assert_eq!(
            d.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn typed_ids_preserve_bytes_but_not_type() {
        let digest = Digest::hash(b"same bytes");
        let blob = BlobId::new(digest);
        let directory = DirectoryId::new(digest);

        assert_eq!(blob.digest(), directory.digest());
        assert_eq!(blob.to_string(), digest.to_string());
        assert_eq!(blob, blob.to_string().parse().unwrap());
        assert_eq!(directory, directory.to_string().parse().unwrap());
    }

    #[test]
    fn chunk_ids_preserve_bytes() {
        let digest = Digest::hash(b"chunk bytes");
        let chunk = ChunkId::new(digest);

        assert_eq!(chunk.digest(), digest);
        assert_eq!(chunk, chunk.to_string().parse().unwrap());
    }

    #[test]
    fn fromstr_rejects_garbage_base64() {
        // the `blake3-` prefix is present but the body is not base64url at all.
        assert!(matches!(
            "blake3-!!!!".parse::<Digest>().unwrap_err(),
            DigestError::InvalidBase64(_)
        ));
    }

    #[test]
    fn fromstr_valid_base64_wrong_length_rejected() {
        // "AAAA" decodes cleanly, but to three bytes rather than DIGEST_LEN.
        assert_eq!(
            "blake3-AAAA".parse::<Digest>().unwrap_err(),
            DigestError::InvalidLength(3)
        );
    }

    #[test]
    fn from_hex_rejects_non_hex() {
        assert!(matches!(
            Digest::from_hex("zzzz").unwrap_err(),
            DigestError::InvalidHex(_)
        ));
    }

    #[test]
    fn from_hex_valid_but_wrong_length_rejected() {
        // valid lowercase hex, but two bytes rather than DIGEST_LEN.
        assert_eq!(
            Digest::from_hex("abcd").unwrap_err(),
            DigestError::InvalidLength(2)
        );
    }

    #[test]
    fn object_id_display_digest_and_conversions() {
        let digest = Digest::hash(b"object");
        let blob = BlobId::new(digest);
        let directory = DirectoryId::new(digest);

        let blob_obj = ObjectId::from(blob);
        let dir_obj = ObjectId::from(directory);
        assert_eq!(blob_obj, ObjectId::Blob(blob));
        assert_eq!(dir_obj, ObjectId::Directory(directory));

        // Display tags the kind ahead of the shared digest text.
        assert_eq!(blob_obj.to_string(), format!("blob {digest}"));
        assert_eq!(dir_obj.to_string(), format!("directory {digest}"));

        // the inner digest is recoverable and identical across both kinds.
        assert_eq!(blob_obj.digest(), digest);
        assert_eq!(dir_obj.digest(), digest);
    }

    #[test]
    fn object_id_orders_blob_before_directory() {
        let digest = Digest::hash(b"same bytes, different kind");
        let blob = ObjectId::Blob(BlobId::new(digest));
        let directory = ObjectId::Directory(DirectoryId::new(digest));
        assert_ne!(blob, directory);
        // the declared variant order (Blob before Directory) drives Ord even
        // when the underlying digest is identical.
        assert!(blob < directory);
    }
}
