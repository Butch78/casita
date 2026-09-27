//! An in-memory [`BlobStore`], backed by a `HashMap`. Intended for tests and
//! ephemeral use.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;

use super::{BlobBatchGuard, BlobReader, BlobStore, BlobWriter};
use crate::digest::{BlobId, ChunkId};
use crate::error::Error;
use crate::metadata::{DataPinLease, PinBindings, PinResource, WritePins};

/// A [`BlobStore`] that keeps every blob in memory.
#[derive(Clone, Default)]
pub struct MemoryBlobStore {
    db: Arc<RwLock<HashMap<BlobId, Bytes>>>,
    pins: PinBindings,
}

impl MemoryBlobStore {
    /// Create an empty in-memory blob service.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    fn publication(&self) -> super::PayloadPublication<'_> {
        super::PayloadPublication::Immediate
    }

    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.pins.write_scope()
    }

    fn begin_pinned_batch(&self, pin: DataPinLease) -> Result<BlobBatchGuard, Error> {
        self.pins.attach(&pin);
        Ok(BlobBatchGuard::default().with_pin(pin))
    }

    async fn nar_available(&self, digest: &BlobId) -> Result<bool, Error> {
        self.has(digest).await
    }

    async fn has(&self, digest: &BlobId) -> Result<bool, Error> {
        Ok(self.db.read().unwrap().contains_key(digest))
    }

    async fn open_proof(
        &self,
        digest: &BlobId,
        size: u64,
    ) -> Result<Option<Box<dyn super::BlobStreamReader>>, Error> {
        let Some(bytes) = self.db.read().unwrap().get(digest).cloned() else {
            return Ok(None);
        };
        if bytes.len() as u64 != size {
            return Err(io::Error::other("blob size mismatch").into());
        }
        let (data, root) = crate::verified::build_outboard(bytes.clone()).await?;
        if root != *digest {
            return Err(io::Error::other("blob digest mismatch").into());
        }
        let outboard = bao_tree::io::outboard::PreOrderOutboard {
            root: bao_tree::blake3::Hash::from(*digest.digest().as_bytes()),
            tree: bao_tree::BaoTree::new(size, crate::verified::BLOCK_SIZE),
            data,
        };
        Ok(Some(crate::verified::stream::produced(
            move |writer| async move {
                bao_tree::io::fsm::encode_ranges_validated(
                    bytes,
                    outboard,
                    &bao_tree::ChunkRanges::all(),
                    iroh_io::TokioStreamWriter(writer),
                )
                .await
                .map_err(io::Error::other)
            },
        )))
    }

    async fn open_proof_scoped(
        &self,
        digest: &BlobId,
        size: u64,
        _pin: DataPinLease,
        _catalog: Option<&[u8]>,
    ) -> Result<Option<Box<dyn super::BlobStreamReader>>, Error> {
        self.open_proof(digest, size).await
    }

    async fn open_read(&self, digest: &BlobId) -> Result<Option<Box<dyn BlobReader>>, Error> {
        Ok(self
            .db
            .read()
            .unwrap()
            .get(digest)
            .cloned()
            .map(|b| Box::new(io::Cursor::new(b)) as Box<dyn BlobReader>))
    }

    async fn open_read_scoped(
        &self,
        digest: &BlobId,
        _pin: DataPinLease,
        _catalog: Option<&[u8]>,
    ) -> Result<Option<Box<dyn BlobReader>>, Error> {
        // The ordinary memory reader owns its immutable bytes; no lazy I/O.
        self.open_read(digest).await
    }

    async fn open_write(&self) -> Box<dyn BlobWriter> {
        Box::new(MemoryBlobWriter {
            db: self.db.clone(),
            buf: Vec::new(),
            done: None,
            pins: self.pins.capture(),
        })
    }

    async fn overwrite(
        &self,
        old: &BlobId,
        size: u64,
        offset: u64,
        replacement: &[u8],
    ) -> Result<(BlobId, Bytes), Error> {
        let range = crate::verified::patch::aligned(size, offset, replacement.len() as u64)?;
        let original = self
            .db
            .read()
            .unwrap()
            .get(old)
            .cloned()
            .ok_or(Error::NotFound {
                digest: (*old).into(),
            })?;
        if original.len() as u64 != size {
            return Err(io::Error::other("overwrite size mismatch").into());
        }
        if replacement.is_empty() {
            return Ok((*old, Bytes::new()));
        }
        let (outboard, root) = crate::verified::build_outboard(original.clone()).await?;
        if root != *old {
            return Err(io::Error::other("overwrite digest mismatch").into());
        }
        let proof = crate::verified::encode_slice(
            original.clone(),
            outboard,
            old,
            size,
            range.start,
            range.end - range.start,
        )
        .await?;
        let mut changed = original.to_vec();
        changed[offset as usize..offset as usize + replacement.len()].copy_from_slice(replacement);
        Ok((self.put_slice(&changed).await?, proof.into()))
    }
}

/// Writer for [`MemoryBlobStore`]: buffers bytes, then hashes and inserts on
/// [`BlobWriter::close`].
struct MemoryBlobWriter {
    db: Arc<RwLock<HashMap<BlobId, Bytes>>>,
    buf: Vec<u8>,
    /// Set once closed, so `close` is idempotent.
    done: Option<(BlobId, u64)>,
    pins: WritePins,
}

impl tokio::io::AsyncWrite for MemoryBlobWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.done.is_some() {
            // accepting bytes after close would silently drop them from the
            // finished blob; match ChunkedBlobWriter and refuse.
            return Poll::Ready(Err(io::Error::other("write after blob writer closed")));
        }
        this.buf.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[async_trait]
impl BlobWriter for MemoryBlobWriter {
    async fn close(&mut self) -> Result<(BlobId, u64), Error> {
        if let Some(done) = self.done {
            return Ok(done);
        }
        let size = self.buf.len() as u64;
        let digest = BlobId::new(blake3::hash(&self.buf).into());
        let bytes = Bytes::copy_from_slice(&self.buf);
        let db = self.db.clone();
        self.pins
            .clone()
            .write(
                std::collections::BTreeSet::from([PinResource::Blob(digest)]),
                async move {
                    db.write().unwrap().insert(digest, bytes);
                    Ok(())
                },
            )
            .await?;
        self.buf.clear();
        self.done = Some((digest, size));
        Ok((digest, size))
    }
}

#[async_trait]
impl crate::blob::BlobGc for MemoryBlobStore {
    async fn reclaim_metadata_pinned(
        &self,
        _pins: Arc<dyn crate::metadata::PinStore>,
        _owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn list_blobs(&self) -> futures::stream::BoxStream<'_, Result<BlobId, Error>> {
        let digests: Vec<BlobId> = self.db.read().unwrap().keys().copied().collect();
        Box::pin(futures::stream::iter(
            digests.into_iter().map(Ok::<BlobId, Error>),
        ))
    }

    fn list_chunks(&self) -> futures::stream::BoxStream<'_, Result<ChunkId, Error>> {
        // the in-memory backend stores whole blobs; it has no separate chunks.
        Box::pin(futures::stream::empty())
    }

    async fn delete_blob(&self, digest: &BlobId) -> Result<(), Error> {
        self.db.write().unwrap().remove(digest);
        Ok(())
    }

    async fn delete_chunk(&self, _digest: &ChunkId) -> Result<(), Error> {
        Ok(())
    }

    // Each payload is one map entry, and an open reader owns its own copy of
    // the bytes. No physical path is shared or pinned, so the collector's
    // logical claims already make these deletions safe.
    async fn delete_blobs_pinned(
        &self,
        digests: &[BlobId],
        _pins: Arc<dyn crate::metadata::PinStore>,
        _owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
        _before_prune: bool,
    ) -> Result<usize, Error> {
        let mut db = self.db.write().unwrap();
        for digest in digests {
            db.remove(digest);
        }
        Ok(digests.len())
    }

    async fn delete_chunks_pinned(
        &self,
        digests: &[ChunkId],
        _pins: Arc<dyn crate::metadata::PinStore>,
        _owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<usize, Error> {
        Ok(digests.len())
    }

    async fn finish_deletions_pinned(
        &self,
        _force_reclaim: bool,
        _pins: Arc<dyn crate::metadata::PinStore>,
        _owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
        _before_prune: bool,
    ) -> Result<(), Error> {
        Ok(())
    }

    async fn finish_collection_pinned(
        &self,
        _force_reclaim: bool,
        _pins: Arc<dyn crate::metadata::PinStore>,
        _owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::native::{read_blob, write_blob};
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

    #[tokio::test]
    async fn online_batch_protects_bytes_until_the_last_writer_ends() {
        use crate::metadata::{
            DataPin, MemoryPinStore, PinScope, PinStore, flush_repository_leases,
        };
        use std::collections::BTreeSet;

        let ledger = Arc::new(MemoryPinStore::default());
        let pin = DataPinLease::acquire(
            ledger.clone(),
            DataPin {
                scope: PinScope::Staging,
                catalog: None,
                resources: BTreeSet::new(),
            },
        )
        .await
        .unwrap();
        let store = MemoryBlobStore::new();
        let batch = store.begin_pinned_batch(pin).unwrap();
        let mut writer = store.open_write().await;
        writer.write_all(b"staged").await.unwrap();
        // A submitted writer retains the operation after its batch ends.
        drop(batch);
        let (blob, _) = writer.close().await.unwrap();
        let resources = BTreeSet::from([PinResource::Blob(blob)]);
        let before = ledger.inventory().await.unwrap();
        assert!(
            ledger
                .claim_deletions(before.revision, resources.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.has(&blob).await.unwrap());
        let garbage = BTreeSet::from([PinResource::Blob(BlobId::new(crate::Digest::hash(
            b"garbage",
        )))]);
        let deletion = ledger
            .claim_deletions(before.revision, garbage)
            .await
            .unwrap()
            .unwrap();
        ledger.finish_deletions(&deletion).await.unwrap();
        drop(writer);
        flush_repository_leases().await.unwrap();
        let after = ledger.inventory().await.unwrap();
        assert!(after.pins.is_empty());
        let deletion = ledger
            .claim_deletions(after.revision, resources)
            .await
            .unwrap()
            .unwrap();
        ledger.finish_deletions(&deletion).await.unwrap();
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let svc = MemoryBlobStore::new();
        let big = vec![0xabu8; 100_000];
        for data in [
            b"".as_slice(),
            b"x".as_slice(),
            b"hello world".as_slice(),
            big.as_slice(),
        ] {
            let digest = write_blob(&svc, data).await;
            // blob identity is BLAKE3 of the full content.
            assert_eq!(digest, BlobId::new(blake3::hash(data).into()));
            assert!(svc.has(&digest).await.unwrap());
            assert_eq!(read_blob(&svc, &digest).await.as_deref(), Some(data));
        }
    }

    #[tokio::test]
    async fn has_batch_reports_presence_in_input_order() {
        let svc = MemoryBlobStore::new();
        let present = write_blob(&svc, b"here").await;
        let absent = BlobId::new(blake3::hash(b"absent").into());
        assert_eq!(
            svc.has_batch(&[absent, present, absent]).await.unwrap(),
            vec![false, true, false]
        );
        assert_eq!(svc.has_batch(&[]).await.unwrap(), Vec::<bool>::new());
    }

    #[tokio::test]
    async fn missing_blob_reads_none() {
        let svc = MemoryBlobStore::new();
        let digest = BlobId::new(blake3::hash(b"absent").into());
        assert!(!svc.has(&digest).await.unwrap());
        assert!(svc.open_read(&digest).await.unwrap().is_none());
        assert_eq!(svc.chunks(&digest).await.unwrap(), None);
    }

    #[tokio::test]
    async fn present_blob_has_empty_chunk_list() {
        let svc = MemoryBlobStore::new();
        let digest = write_blob(&svc, b"data").await;
        assert_eq!(svc.chunks(&digest).await.unwrap(), Some(vec![]));
    }

    #[tokio::test]
    async fn reader_can_seek() {
        let svc = MemoryBlobStore::new();
        let digest = write_blob(&svc, b"0123456789").await;
        let mut r = svc.open_read(&digest).await.unwrap().unwrap();
        r.seek(io::SeekFrom::Start(4)).await.unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"456789");
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let svc = MemoryBlobStore::new();
        let mut w = svc.open_write().await;
        w.write_all(b"abc").await.unwrap();
        let (d1, s1) = w.close().await.unwrap();
        let (d2, s2) = w.close().await.unwrap();
        assert_eq!((d1, s1), (d2, s2));
        assert_eq!(s1, 3);
        // exactly one entry stored.
        assert!(svc.has(&d1).await.unwrap());
    }

    #[tokio::test]
    async fn write_after_close_errors() {
        let svc = MemoryBlobStore::new();
        let mut w = svc.open_write().await;
        w.write_all(b"abc").await.unwrap();
        w.close().await.unwrap();
        // writing after close must fail rather than silently drop the bytes.
        assert!(w.write_all(b"def").await.is_err());
    }
}
