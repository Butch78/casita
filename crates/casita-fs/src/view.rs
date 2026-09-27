//! Read-only, byte-safe filesystem view over an Casita store.
//!
//! This module contains no FUSE or platform API. Frontends translate their
//! request protocol into these operations, which keeps the casita traversal
//! identical between every transport that serves the store.

use std::collections::BTreeMap;
use std::io::SeekFrom;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use casita::{BlobId, Digest, Directory, DirectoryId, Node};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

/// A seekable content stream whose owner retains the underlying repository data.
pub trait ContentStream:
    tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin + 'static
{
}

impl<T> ContentStream for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncSeek + Send + Unpin + 'static
{
}

/// Repository-scoped content access used by filesystem views and builders.
///
/// Implementations own whatever snapshot or retention hold makes returned
/// objects safe to read. Callers therefore never receive raw physical stores
/// or perform global hash lookups themselves.
#[async_trait]
pub trait ContentReader: Send + Sync {
    /// Read one committed canonical directory.
    async fn directory(&self, digest: &DirectoryId) -> std::io::Result<Option<Directory>>;

    /// Open one committed blob for ranged reads.
    async fn open_blob(&self, digest: &BlobId) -> std::io::Result<Option<Box<dyn ContentStream>>>;

    /// Materialize a committed directory closure at an empty destination.
    async fn checkout_directory(&self, digest: &DirectoryId, target: &Path) -> std::io::Result<()>;
}

/// Use Casita's supported application repository directly in filesystem views.
#[async_trait]
impl ContentReader for casita::Repository {
    async fn directory(&self, digest: &DirectoryId) -> std::io::Result<Option<Directory>> {
        let Some(mut reader) = self
            .open(&casita::ObjectKey::directory(*digest))
            .await
            .map_err(std::io::Error::other)?
        else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Directory::decode(&bytes)
            .map(Some)
            .map_err(std::io::Error::other)
    }
    async fn open_blob(&self, digest: &BlobId) -> std::io::Result<Option<Box<dyn ContentStream>>> {
        Ok(self
            .open(&casita::ObjectKey::blob(*digest))
            .await
            .map_err(std::io::Error::other)?
            .map(|reader| Box::new(reader) as Box<dyn ContentStream>))
    }
    async fn checkout_directory(&self, digest: &DirectoryId, target: &Path) -> std::io::Result<()> {
        self.checkout(&casita::ObjectKey::directory(*digest), target)
            .await
            .map_err(std::io::Error::other)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemEntry {
    pub name: Vec<u8>,
    pub node: FilesystemNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemNodeKind {
    Regular,
    Directory,
    Symlink,
}

/// Opaque handle to one node in a [`FilesystemView`].
///
/// Frontends can inspect filesystem attributes and pass the handle back to
/// the view for traversal or reads, without depending on Casita's storage
/// model. The underlying digest remains an implementation detail of the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemNode(Node);

/// The identity of a node's *content*.
///
/// A frontend keys its inode table on this so that identical content anywhere
/// in the store shares one inode: a store path referenced from twenty closures
/// is cached by the kernel once. Because the store is a Merkle DAG rather than
/// a tree, this also rules out allocating inode ranges per directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentKey {
    Regular { digest: Digest, executable: bool },
    Directory { digest: Digest },
    Symlink { target: Vec<u8> },
}

impl FilesystemNode {
    #[must_use]
    pub fn from_casita(node: Node) -> Self {
        Self(node)
    }

    #[must_use]
    pub fn kind(&self) -> FilesystemNodeKind {
        node_kind(&self.0)
    }

    /// File length, symlink-target length, or zero for a directory.
    #[must_use]
    pub fn size(&self) -> u64 {
        match &self.0 {
            Node::File { size, .. } => *size,
            Node::Symlink { target } => target.as_bytes().len() as u64,
            Node::Directory { .. } => 0,
        }
    }

    #[must_use]
    pub fn executable(&self) -> bool {
        matches!(
            &self.0,
            Node::File {
                executable: true,
                ..
            }
        )
    }

    #[must_use]
    pub fn symlink_target(&self) -> Option<&[u8]> {
        match &self.0 {
            Node::Symlink { target } => Some(target.as_bytes()),
            _ => None,
        }
    }

    #[must_use]
    pub fn content_key(&self) -> ContentKey {
        match &self.0 {
            Node::File {
                digest, executable, ..
            } => ContentKey::Regular {
                digest: *digest.as_digest(),
                executable: *executable,
            },
            Node::Directory { digest, .. } => ContentKey::Directory {
                digest: *digest.as_digest(),
            },
            Node::Symlink { target } => ContentKey::Symlink {
                target: target.as_bytes().to_vec(),
            },
        }
    }
}

impl FilesystemEntry {
    #[must_use]
    pub fn kind(&self) -> FilesystemNodeKind {
        self.node.kind()
    }
}

#[must_use]
pub fn node_kind(node: &Node) -> FilesystemNodeKind {
    match node {
        Node::File { .. } => FilesystemNodeKind::Regular,
        Node::Directory { .. } => FilesystemNodeKind::Directory,
        Node::Symlink { .. } => FilesystemNodeKind::Symlink,
    }
}

/// A snapshot of the store-path roots a frontend may serve, with live access
/// to their casita content.
#[derive(Clone)]
pub struct FilesystemView {
    content: Arc<dyn ContentReader>,
    roots: Arc<BTreeMap<Vec<u8>, Node>>,
}

impl FilesystemView {
    pub fn new(content: Arc<dyn ContentReader>, roots: BTreeMap<Vec<u8>, Node>) -> Self {
        Self {
            content,
            roots: Arc::new(roots),
        }
    }

    pub fn root(&self, name: &[u8]) -> Option<FilesystemNode> {
        self.roots.get(name).cloned().map(FilesystemNode)
    }

    pub fn roots(&self) -> Vec<FilesystemEntry> {
        self.roots
            .iter()
            .map(|(name, node)| FilesystemEntry {
                name: name.clone(),
                node: FilesystemNode(node.clone()),
            })
            .collect()
    }

    pub async fn lookup(
        &self,
        directory: &FilesystemNode,
        name: &[u8],
    ) -> Result<Option<FilesystemNode>> {
        let Node::Directory { digest, .. } = &directory.0 else {
            bail!("not a directory")
        };
        let directory = self
            .content
            .directory(digest)
            .await?
            .ok_or_else(|| anyhow!("directory missing from casita: {digest}"))?;
        Ok(directory.get(name).cloned().map(FilesystemNode))
    }

    pub async fn entries(&self, directory: &FilesystemNode) -> Result<Vec<FilesystemEntry>> {
        let Node::Directory { digest, .. } = &directory.0 else {
            bail!("not a directory")
        };
        let directory = self
            .content
            .directory(digest)
            .await?
            .ok_or_else(|| anyhow!("directory missing from casita: {digest}"))?;
        Ok(directory
            .nodes()
            .map(|(name, node)| FilesystemEntry {
                name: name.as_bytes().to_vec(),
                node: FilesystemNode(node.clone()),
            })
            .collect())
    }

    /// Open a file node for reading.
    ///
    /// The reader seeks, so a frontend holding one per open file handle serves
    /// arbitrary read offsets without re-opening the blob (and, for a chunked
    /// or remote blob store, without refetching what it already has).
    pub async fn open(&self, file: &FilesystemNode) -> Result<Box<dyn ContentStream>> {
        let Node::File { digest, .. } = &file.0 else {
            bail!("not a regular file")
        };
        self.content
            .open_blob(digest)
            .await?
            .ok_or_else(|| anyhow!("blob missing from casita: {digest}"))
    }

    pub async fn read(&self, file: &FilesystemNode, offset: u64, size: u32) -> Result<Vec<u8>> {
        let Node::File {
            size: file_size, ..
        } = &file.0
        else {
            bail!("not a regular file")
        };
        if offset >= *file_size || size == 0 {
            return Ok(Vec::new());
        }
        let wanted = (*file_size - offset).min(u64::from(size)) as usize;
        let mut reader = self.open(file).await?;
        reader.seek(SeekFrom::Start(offset)).await?;
        let mut bytes = vec![0; wanted];
        reader.read_exact(&mut bytes).await?;
        Ok(bytes)
    }
}
