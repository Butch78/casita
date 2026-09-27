//! Bao outboard persistence and integrity-checked range reads.

use std::io;
use std::sync::Arc;

use bytes::Bytes;
use futures::stream::BoxStream;
use object_store::ObjectStoreExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{ChunkedBlobStore, put_object};
use crate::blob::chunked_reader::{ChunkSource, ChunkedReader};
use crate::blob::{BlobIntegrityError, BlobStore, ChunkMeta};
use crate::digest::BlobId;
use crate::error::Error;

/// BLAKE3 verified streaming support: persisting bao outboards and doing
/// integrity-checked range reads.
impl ChunkedBlobStore {
    /// Store the bao outboard for a blob.
    pub async fn put_outboard(&self, digest: &BlobId, outboard: Bytes) -> Result<(), Error> {
        let bytes = if outboard.len() > super::pages::LEAF_BYTES {
            super::pages::Pages::from(self)
                .build_bytes(&mut outboard.as_ref())
                .await?
                .encode()
                .into()
        } else {
            outboard
        };
        self.put_outboard_root(digest, bytes).await
    }

    pub(super) async fn put_outboard_root(
        &self,
        digest: &BlobId,
        bytes: Bytes,
    ) -> Result<(), Error> {
        if let Some(packed) = self
            .packed_chunks
            .as_ref()
            .filter(|packed| packed.uses_state_catalog())
        {
            packed.put_sidecar(*digest, bytes).await?;
        } else {
            put_object(
                &self.object_store,
                &self.outboard_path(digest),
                bytes,
                self.immutable_cache,
            )
            .await
            .map_err(io::Error::other)?;
        }
        Ok(())
    }

    pub(super) async fn packed_outboard_root(
        &self,
        digest: &BlobId,
        catalog: Option<&[u8]>,
    ) -> io::Result<Option<Bytes>> {
        match &self.packed_chunks {
            Some(packed) => packed.sidecar(*digest, catalog).await,
            None => Ok(None),
        }
    }

    /// Fetch the stored Bao outboard. Corrupt selected packs never fall back to loose data.
    pub async fn get_outboard(&self, digest: &BlobId) -> Result<Option<Bytes>, Error> {
        let bytes = match self.packed_outboard_root(digest, None).await? {
            Some(bytes) => bytes,
            None => match self.object_store.get(&self.outboard_path(digest)).await {
                Ok(res) => res.bytes().await.map_err(io::Error::other)?,
                Err(object_store::Error::NotFound { .. }) => return Ok(None),
                Err(error) => return Err(io::Error::other(error).into()),
            },
        };
        if let Some(root) = super::pages::Root::decode(&bytes)? {
            if root.kind != super::pages::OUTBOARD {
                return Err(super::pages::invalid().into());
            }
            let mut cursor = super::pages::Cursor::new(self, root).await?;
            use iroh_io::AsyncSliceReader;
            return Ok(Some(
                cursor
                    .read_at(0, usize::try_from(root.span).map_err(io::Error::other)?)
                    .await?,
            ));
        }
        Ok(Some(bytes))
    }

    /// Stream a blob to compute its bao outboard without changing storage.
    ///
    /// The blob is not buffered in memory. Callers that want to repair an
    /// existing outboard can compare this value to the stored one before
    /// choosing to publish it.
    pub async fn compute_outboard(&self, digest: &BlobId) -> Result<Bytes, Error> {
        let chunks = BlobStore::chunks(self, digest)
            .await?
            .ok_or(Error::NotFound {
                digest: (*digest).into(),
            })?;
        let reader = BlobSliceReader::new(self.clone(), chunks);
        let (outboard, root) = crate::verified::build_outboard(reader).await?;
        // the manifest must actually describe this blob: if the streamed
        // content's bao root is not `digest`, the manifest is lying (e.g. it was
        // substituted for another blob's), so refuse rather than persist an
        // outboard for the wrong content under a valid key.
        if root != *digest {
            return Err(io::Error::other(BlobIntegrityError::Blob { expected: *digest }).into());
        }
        Ok(outboard)
    }

    /// Stream a blob to compute its bao outboard and persist it, returning the
    /// outboard. The blob is not buffered in memory.
    pub(crate) async fn build_outboard(&self, digest: &BlobId) -> Result<Bytes, Error> {
        let outboard = self.compute_outboard(digest).await?;
        self.put_outboard(digest, outboard.clone()).await?;
        Ok(outboard)
    }

    async fn verified_source(&self, digest: &BlobId) -> Result<(BlobSliceReader, Bytes), Error> {
        let (outboard, chunks) = tokio::try_join!(self.get_outboard(digest), async {
            BlobStore::chunks(self, digest)
                .await?
                .ok_or(Error::NotFound {
                    digest: (*digest).into(),
                })
        },)?;
        let reader = BlobSliceReader::new(self.clone(), chunks);
        let tree = bao_tree::BaoTree::new(reader.size, crate::verified::BLOCK_SIZE);
        let outboard = match outboard {
            Some(bytes) => bytes,
            None if tree.outboard_size() == 0 => Bytes::new(),
            None => return Err(Error::Msg(format!("no outboard stored for blob {digest}"))),
        };
        Ok((reader, outboard))
    }

    /// Verified read of `[offset, offset + len)`, checked against `digest`.
    ///
    /// Streams only the covered chunks from the store (the blob is not buffered)
    /// and uses proof metadata generated during ingestion. Fails if the range
    /// does not verify or reaches past the end of the blob (an empty range is
    /// valid at any in-range offset).
    pub async fn verified_read(
        &self,
        digest: &BlobId,
        offset: u64,
        len: u64,
    ) -> Result<Bytes, Error> {
        let (reader, outboard) = self.verified_source(digest).await?;
        let size = reader.size;
        // encode_slice and decode_slice both enforce the range rule (and both
        // short-circuit an empty range), so nothing is restated here.
        let slice =
            crate::verified::encode_slice(reader, outboard, digest, size, offset, len).await?;
        crate::verified::decode_slice(&slice, digest, size, offset, len).await
    }

    /// Stream a blob as independently Bao-verified windows.
    ///
    /// Unlike a normal sequential [`BlobStore::open_read`], which verifies the
    /// whole blob at EOF, every item returned here has already been verified
    /// against `digest`. The outboard and chunk map are loaded once and the
    /// underlying chunk reader is retained across windows.
    pub async fn verified_stream(
        &self,
        digest: &BlobId,
        window_bytes: u64,
    ) -> Result<BoxStream<'static, Result<Bytes, Error>>, Error> {
        if window_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "verified stream window must be nonzero",
            )
            .into());
        }
        let (reader, outboard) = self.verified_source(digest).await?;
        let size = reader.size;
        let state = VerifiedStreamState {
            reader,
            outboard,
            digest: *digest,
            size,
            position: 0,
            window_bytes,
        };
        Ok(Box::pin(futures::stream::try_unfold(
            state,
            |mut state| async move {
                if state.position == state.size {
                    return Ok(None);
                }
                let len = state
                    .window_bytes
                    .min(state.size.saturating_sub(state.position));
                let encoded = crate::verified::encode_slice(
                    &mut state.reader,
                    state.outboard.clone(),
                    &state.digest,
                    state.size,
                    state.position,
                    len,
                )
                .await?;
                let verified = crate::verified::decode_slice(
                    &encoded,
                    &state.digest,
                    state.size,
                    state.position,
                    len,
                )
                .await?;
                state.position += len;
                Ok(Some((verified, state)))
            },
        )))
    }
}

struct VerifiedStreamState {
    reader: BlobSliceReader,
    outboard: Bytes,
    digest: BlobId,
    size: u64,
    position: u64,
    window_bytes: u64,
}

/// An [`iroh_io::AsyncSliceReader`] over a stored blob's chunk list, used to
/// feed bao-tree without materializing the whole blob. Assembles the chunks
/// with a lazily-built [`ChunkedReader`], caching it across reads.
pub(super) struct BlobSliceReader {
    store: ChunkedBlobStore,
    chunks: Vec<ChunkMeta>,
    size: u64,
    reader: Option<ChunkedReader>,
}

impl BlobSliceReader {
    pub(super) fn new(store: ChunkedBlobStore, chunks: Vec<ChunkMeta>) -> Self {
        let size = chunks.iter().map(|c| c.size).sum();
        Self {
            store,
            chunks,
            size,
            reader: None,
        }
    }
}

impl iroh_io::AsyncSliceReader for BlobSliceReader {
    async fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Bytes> {
        let size = self.size;
        let reader = self.reader.get_or_insert_with(|| {
            let source: Arc<dyn ChunkSource> = Arc::new(self.store.clone());
            // bao-tree verifies the covered range against the digest itself, so
            // this reader does not need the whole-blob EOF check (and it seeks,
            // which would disable it anyway).
            ChunkedReader::new(source, self.chunks.iter().map(|c| (c.digest, c.size)), None)
        });
        reader.seek(io::SeekFrom::Start(offset)).await?;

        let to_read = len.min(size.saturating_sub(offset) as usize);
        let mut buf = vec![0u8; to_read];
        reader.read_exact(&mut buf).await?;
        Ok(Bytes::from(buf))
    }

    async fn size(&mut self) -> io::Result<u64> {
        Ok(self.size)
    }
}
