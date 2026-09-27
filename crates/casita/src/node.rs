//! [`Node`], a single edge in the Merkle DAG.

use crate::digest::{BlobId, DirectoryId, ObjectId};
use crate::path::SymlinkTarget;

/// A node in the content-addressed tree.
///
/// Nodes are *nameless*: the name lives in the containing [`crate::Directory`]
/// (or is supplied as a root-node label). A node is one of three kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A pointer to a child [`crate::Directory`] by its digest.
    ///
    /// `size` is the directory's recursive descendant count (see
    /// [`crate::Directory::size`]); it is a verifiable Merkle-tree edge.
    Directory {
        /// Typed identifier of the referenced directory.
        digest: DirectoryId,
        /// Recursive descendant count of the referenced directory.
        size: u64,
    },
    /// A pointer to a blob (regular file) by its digest.
    File {
        /// Typed identifier of the blob (BLAKE3 of the file contents).
        digest: BlobId,
        /// Size of the blob in bytes.
        size: u64,
        /// Whether the file has the executable bit set.
        executable: bool,
    },
    /// A symbolic link with an inline target.
    Symlink {
        /// The link target.
        target: SymlinkTarget,
    },
}

impl Node {
    /// The content digest the node points at, or `None` for a symlink (its
    /// target is inline; it references no stored content).
    pub fn digest(&self) -> Option<ObjectId> {
        match self {
            Node::Directory { digest, .. } => Some((*digest).into()),
            Node::File { digest, .. } => Some((*digest).into()),
            Node::Symlink { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_node_points_at_a_blob() {
        let id = BlobId::new([7u8; 32].into());
        let node = Node::File {
            digest: id,
            size: 3,
            executable: false,
        };
        assert_eq!(node.digest(), Some(ObjectId::Blob(id)));
    }

    #[test]
    fn directory_node_points_at_a_directory() {
        let id = DirectoryId::new([4u8; 32].into());
        let node = Node::Directory {
            digest: id,
            size: 9,
        };
        assert_eq!(node.digest(), Some(ObjectId::Directory(id)));
    }

    #[test]
    fn symlink_node_references_no_stored_content() {
        // a symlink carries its target inline, so it addresses nothing.
        let node = Node::Symlink {
            target: SymlinkTarget::try_from("../elsewhere").unwrap(),
        };
        assert_eq!(node.digest(), None);
    }
}
