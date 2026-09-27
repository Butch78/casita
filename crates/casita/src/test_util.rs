//! Helpers shared by unit tests across modules.
//!
//! Everything directly in this module is portable: it compiles under
//! `--no-default-features`, where the tests for the identity, format, and
//! encoding layers still run. Helpers that need a local payload store or
//! Tokio I/O live in [`native`], behind the `native` feature, so enabling no
//! features cannot pull a native dependency into a portable test build.

use crate::path::PathComponent;

/// A [`PathComponent`] from a literal, panicking on invalid input.
pub(crate) fn pc(s: &str) -> PathComponent {
    PathComponent::try_from(s).unwrap()
}

#[cfg(feature = "native")]
pub(crate) mod native {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::blob::{BlobStore, ChunkedBlobStore};
    use crate::digest::BlobId;

    /// Write `data` as a blob, returning its digest.
    pub(crate) async fn write_blob(store: &impl BlobStore, data: &[u8]) -> BlobId {
        let mut w = store.open_write().await;
        w.write_all(data).await.unwrap();
        let (digest, size) = w.close().await.unwrap();
        assert_eq!(size, data.len() as u64);
        digest
    }

    /// Read a blob fully, or `None` if it is absent.
    pub(crate) async fn read_blob(store: &impl BlobStore, digest: &BlobId) -> Option<Vec<u8>> {
        let mut r = store.open_read(digest).await.unwrap()?;
        let mut v = Vec::new();
        r.read_to_end(&mut v).await.unwrap();
        Some(v)
    }

    /// A [`ChunkedBlobStore`] over a fresh temp dir with a deliberately small
    /// average chunk size, so tests exercise multi-chunk blobs and deduplication.
    pub(crate) fn small_chunked_store() -> (ChunkedBlobStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let fs = object_store::local::LocalFileSystem::new_with_prefix(dir.path()).unwrap();
        let store = ChunkedBlobStore::new(
            std::sync::Arc::new(fs),
            object_store::path::Path::default(),
            1024,
        );
        (store, dir)
    }
}
