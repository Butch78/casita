//! Native repository fixtures shared by filesystem and builder tests.
use crate::{ContentReader, ContentStream};
use casita::experimental as native;
use casita::{BlobId, Directory, DirectoryId, Node};
use std::{
    io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct TestRepository(native::Repository<native::MemoryBlobStore, native::MemoryMetadataStore>);
impl Default for TestRepository {
    fn default() -> Self {
        Self(native::Repository::memory().unwrap())
    }
}
#[async_trait::async_trait]
impl ContentReader for TestRepository {
    async fn directory(&self, digest: &DirectoryId) -> io::Result<Option<Directory>> {
        let hold = self.0.retention_hold().await.map_err(io::Error::other)?;
        let Some((_, mut reader)) = hold
            .open_payload(&native::ObjectKey::directory(*digest))
            .await
            .map_err(io::Error::other)?
        else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes).await?;
        Directory::decode(&bytes)
            .map(Some)
            .map_err(io::Error::other)
    }
    async fn open_blob(&self, digest: &BlobId) -> io::Result<Option<Box<dyn ContentStream>>> {
        let hold = self.0.retention_hold().await.map_err(io::Error::other)?;
        Ok(hold
            .open_payload(&native::ObjectKey::blob(*digest))
            .await
            .map_err(io::Error::other)?
            .map(|(_, reader)| Box::new(reader) as Box<dyn ContentStream>))
    }
    async fn checkout_directory(&self, digest: &DirectoryId, target: &Path) -> io::Result<()> {
        self.0
            .checkout(&native::ObjectKey::directory(*digest), target)
            .await
            .map(|_| ())
            .map_err(io::Error::other)
    }
}
impl TestRepository {
    pub async fn import_output(&self, path: &Path) -> io::Result<Node> {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            let target = std::fs::read_link(path)?;
            return Ok(Node::Symlink {
                target: casita::SymlinkTarget::try_from(bytes::Bytes::copy_from_slice(
                    target.as_os_str().as_encoded_bytes(),
                ))
                .map_err(io::Error::other)?,
            });
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name =
            native::RootName::try_from(format!("fixture/{}", NEXT.fetch_add(1, Ordering::Relaxed)))
                .unwrap();
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.is_file() {
            let session = self.0.mutation_session().await.map_err(io::Error::other)?;
            let mut reader = tokio::fs::File::open(path).await?;
            let staged = session
                .stage_blob_reader(&mut reader)
                .await
                .map_err(io::Error::other)?;
            let target = staged.record().key().clone();
            let digest = BlobId::new(staged.record().payload().digest());
            let size = staged.record().payload_size();
            session
                .publish_rooted(vec![staged], name, target)
                .await
                .map_err(io::Error::other)?;
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            return Ok(Node::File {
                digest,
                size,
                executable,
            });
        }
        let imported = self
            .0
            .import(casita::import::FilesystemImport::new(path, name).reread(true))
            .await
            .map_err(io::Error::other)?;
        let digest = DirectoryId::new(imported.native_digest().unwrap());
        let directory = self.directory(&digest).await?.unwrap();
        Ok(Node::Directory {
            digest,
            size: directory.size(),
        })
    }
}
