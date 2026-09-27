//! Validated names used in the data model.
//!
//! - [`PathComponent`] — a single filesystem name (one path element).
//! - [`SymlinkTarget`] — the target string of a symlink.

use bytes::Bytes;

/// Maximum length, in bytes, of a single [`PathComponent`].
pub const MAX_NAME_LEN: usize = 255;

/// Maximum length, in bytes, of a [`SymlinkTarget`].
pub const MAX_TARGET_LEN: usize = 4095;

/// Reasons a byte string is not a valid [`PathComponent`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathComponentError {
    /// The name is empty.
    #[error("path component is empty")]
    Empty,
    /// The name is exactly `..`.
    #[error("path component is `..`")]
    Parent,
    /// The name is exactly `.`.
    #[error("path component is `.`")]
    CurDir,
    /// The name exceeds 255 bytes.
    #[error("path component exceeds {MAX_NAME_LEN} bytes")]
    TooLong,
    /// The name contains a NUL byte.
    #[error("path component contains a NUL byte")]
    Null,
    /// The name contains a `/`.
    #[error("path component contains a slash")]
    Slash,
}

/// Generates the shared boilerplate for a validated byte-string newtype:
/// fallible constructors (via the type's `validate`), byte accessors, and
/// `bstr`-based `Display`/`Debug`.
macro_rules! validated_bytes_impls {
    ($ty:ident, $err:ty) => {
        impl $ty {
            /// The raw bytes.
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }
        }

        impl TryFrom<Bytes> for $ty {
            type Error = $err;

            fn try_from(b: Bytes) -> Result<Self, Self::Error> {
                Self::validate(&b)?;
                Ok(Self(b))
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = $err;

            fn try_from(s: &str) -> Result<Self, Self::Error> {
                Self::validate(s.as_bytes())?;
                Ok(Self(Bytes::copy_from_slice(s.as_bytes())))
            }
        }

        impl TryFrom<String> for $ty {
            type Error = $err;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::validate(s.as_bytes())?;
                Ok(Self(Bytes::from(s.into_bytes())))
            }
        }

        impl std::str::FromStr for $ty {
            type Err = $err;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from(s)
            }
        }

        impl AsRef<[u8]> for $ty {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl From<$ty> for Bytes {
            fn from(v: $ty) -> Bytes {
                v.0
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(bstr::BStr::new(&self.0), f)
            }
        }

        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Debug::fmt(bstr::BStr::new(&self.0), f)
            }
        }
    };
}

/// A validated single path component (a name within a [`crate::Directory`]).
///
/// Guaranteed non-empty, not `.` or `..`, free of `/` and NUL, and at most
/// 255 bytes. Ordering is lexicographic over the raw bytes, which
/// is the canonical sort order for directory entries.
#[repr(transparent)]
#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct PathComponent(Bytes);

validated_bytes_impls!(PathComponent, PathComponentError);

/// Names order as raw bytes, exactly like `[u8]`, so a map keyed by
/// `PathComponent` can be probed with any byte string — this is what lets
/// [`Directory::get`](crate::Directory::get) accept `&str`.
impl std::borrow::Borrow<[u8]> for PathComponent {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl PathComponent {
    fn validate(b: &[u8]) -> Result<(), PathComponentError> {
        if b.is_empty() {
            return Err(PathComponentError::Empty);
        }
        if b == b".." {
            return Err(PathComponentError::Parent);
        }
        if b == b"." {
            return Err(PathComponentError::CurDir);
        }
        if b.len() > MAX_NAME_LEN {
            return Err(PathComponentError::TooLong);
        }
        if b.contains(&0) {
            return Err(PathComponentError::Null);
        }
        if b.contains(&b'/') {
            return Err(PathComponentError::Slash);
        }
        Ok(())
    }
}

/// Reasons a byte string is not a valid [`SymlinkTarget`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SymlinkTargetError {
    /// The target is empty.
    #[error("symlink target is empty")]
    Empty,
    /// The target exceeds 4095 bytes.
    #[error("symlink target exceeds {MAX_TARGET_LEN} bytes")]
    TooLong,
    /// The target contains a NUL byte.
    #[error("symlink target contains a NUL byte")]
    Null,
}

/// A validated symlink target.
///
/// Guaranteed non-empty, free of NUL, and at most 4095 bytes.
/// Unlike [`PathComponent`], slashes, `.`, and `..` are permitted.
#[repr(transparent)]
#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymlinkTarget(Bytes);

validated_bytes_impls!(SymlinkTarget, SymlinkTargetError);

impl SymlinkTarget {
    fn validate(b: &[u8]) -> Result<(), SymlinkTargetError> {
        if b.is_empty() {
            return Err(SymlinkTargetError::Empty);
        }
        if b.len() > MAX_TARGET_LEN {
            return Err(SymlinkTargetError::TooLong);
        }
        if b.contains(&0) {
            return Err(SymlinkTargetError::Null);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in ["a", "foo.txt", "with space", "ünîcøde", ".hidden", "..."] {
            assert!(PathComponent::try_from(n).is_ok(), "{n} should be valid");
        }
    }

    #[test]
    fn invalid_names() {
        assert_eq!(
            PathComponent::try_from("").unwrap_err(),
            PathComponentError::Empty
        );
        assert_eq!(
            PathComponent::try_from(".").unwrap_err(),
            PathComponentError::CurDir
        );
        assert_eq!(
            PathComponent::try_from("..").unwrap_err(),
            PathComponentError::Parent
        );
        assert_eq!(
            PathComponent::try_from("a/b").unwrap_err(),
            PathComponentError::Slash
        );
        assert_eq!(
            PathComponent::try_from("a\0b").unwrap_err(),
            PathComponentError::Null
        );
        assert_eq!(
            PathComponent::try_from("x".repeat(256).as_str()).unwrap_err(),
            PathComponentError::TooLong
        );
    }

    #[test]
    fn parse_idioms_validate() {
        assert!("foo.txt".parse::<PathComponent>().is_ok());
        assert_eq!(
            "a/b".parse::<PathComponent>().unwrap_err(),
            PathComponentError::Slash
        );
        assert!(PathComponent::try_from(String::from("owned")).is_ok());
        assert!("../a".parse::<SymlinkTarget>().is_ok());
        assert_eq!(
            "".parse::<SymlinkTarget>().unwrap_err(),
            SymlinkTargetError::Empty
        );
    }

    #[test]
    fn name_ordering_is_lexicographic() {
        let a = PathComponent::try_from("a").unwrap();
        let b = PathComponent::try_from("b").unwrap();
        let aa = PathComponent::try_from("aa").unwrap();
        assert!(a < aa);
        assert!(aa < b);
    }

    #[test]
    fn symlink_target_allows_slashes_and_dots() {
        assert!(SymlinkTarget::try_from("../a/b").is_ok());
        assert!(SymlinkTarget::try_from(".").is_ok());
        assert!(SymlinkTarget::try_from("..").is_ok());
    }

    #[test]
    fn invalid_symlink_targets() {
        assert_eq!(
            SymlinkTarget::try_from("").unwrap_err(),
            SymlinkTargetError::Empty
        );
        assert_eq!(
            SymlinkTarget::try_from("a\0").unwrap_err(),
            SymlinkTargetError::Null
        );
        assert_eq!(
            SymlinkTarget::try_from("x".repeat(4096).as_str()).unwrap_err(),
            SymlinkTargetError::TooLong
        );
    }
}
