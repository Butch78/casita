//! Versioned on-disk control protocol between the controller and FSKit extension.
//! Control files stay under the repository's security-scoped resource URL.
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

pub const ACTIVE: &str = ".casita-native-active";
pub const LOCK: &str = ".casita-native.lock";
pub const VERSION: &[u8] = b"casita-native-mount-v1\n";

pub fn session(repository: &Path) -> io::Result<PathBuf> {
    let name = std::fs::read_to_string(repository.join(ACTIVE))?;
    if !name.starts_with(".casita-native-session-")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid native mount session",
        ));
    }
    let path = repository.join(name).canonicalize()?;
    if path.parent() != Some(repository.canonicalize()?.as_path())
        || std::fs::read(path.join("version"))? != VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid native mount protocol",
        ));
    }
    Ok(path)
}

pub fn descriptor(session: &Path, name: &[u8]) -> PathBuf {
    session.join(format!("root-{:x}", Sha256::digest(name)))
}

pub fn encode(name: &[u8], node: casita::Node) -> io::Result<Vec<u8>> {
    let component = casita::PathComponent::try_from(bytes::Bytes::copy_from_slice(name))
        .map_err(io::Error::other)?;
    let mut directory = casita::Directory::new();
    directory.add(component, node).map_err(io::Error::other)?;
    Ok(directory.encode())
}

pub fn read(session: &Path, name: &[u8]) -> io::Result<casita::Node> {
    let bytes = std::fs::read(descriptor(session, name))?;
    let directory = casita::Directory::decode(&bytes).map_err(io::Error::other)?;
    if directory.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "publication must contain exactly one root",
        ));
    }
    directory
        .get(name)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "publication name mismatch"))
}

/// The mount helper has not launched yet, so an unmounted target may be retried.
pub fn retryable_launch_failure(code: Option<i32>, stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr);
    code == Some(69)
        && message.contains("Unable to invoke task")
        && (message.contains("communicate with a helper application")
            || message.contains("com.apple.extensionKit.errorDomain error 2"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sessions_reject_escape_and_incompatible_versions() {
        let repository = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let name = ".casita-native-session-test";
        let path = repository.path().join(name);
        std::fs::write(repository.path().join(ACTIVE), name).unwrap();
        std::os::unix::fs::symlink(outside.path(), &path).unwrap();
        std::fs::write(outside.path().join("version"), VERSION).unwrap();
        assert!(session(repository.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("version"), b"unknown").unwrap();
        assert!(session(repository.path()).is_err());
        std::fs::write(path.join("version"), VERSION).unwrap();
        assert_eq!(
            session(repository.path()).unwrap(),
            path.canonicalize().unwrap()
        );
        std::fs::write(repository.path().join(ACTIVE), "../escape").unwrap();
        assert!(session(repository.path()).is_err());
    }

    #[test]
    fn byte_names_roundtrip_and_mismatched_descriptors_fail() {
        let work = tempfile::tempdir().unwrap();
        let node = casita::Node::File {
            digest: casita::BlobId::new(casita::Digest::hash(b"file")),
            size: 4,
            executable: true,
        };
        let name = b"root-\xff";
        std::fs::write(
            descriptor(work.path(), name),
            encode(name, node.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(read(work.path(), name).unwrap(), node);
        std::fs::write(
            descriptor(work.path(), b"other"),
            encode(name, node).unwrap(),
        )
        .unwrap();
        assert!(read(work.path(), b"other").is_err());
        assert!(encode(
            b"../escape",
            casita::Node::Symlink {
                target: "target".try_into().unwrap()
            }
        )
        .is_err());
    }
    #[test]
    fn only_explicit_helper_startup_failures_are_retryable() {
        assert!(retryable_launch_failure(
            Some(69),
            b"mount: Probing resource: Cannot communicate with a helper application\nmount: Unable to invoke task\n"
        ));
        assert!(!retryable_launch_failure(
            Some(69),
            b"mount: permission denied"
        ));
        assert!(!retryable_launch_failure(Some(1), b"Unable to invoke task"));
        assert!(!retryable_launch_failure(None, b"Unable to invoke task"));
    }
}
