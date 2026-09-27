//! BLAKE3 verified streaming (the "bao" scheme), backed by `bao-tree`.
//!
//! BLAKE3 is a Merkle tree, so a blob can be accompanied by an *outboard* — the
//! tree of intermediate hashes — that lets a receiver verify an arbitrary byte
//! range against the blob's root digest **without** having the whole blob. This
//! module is fully streaming: the data source is an [`AsyncSliceReader`], so
//! only the covered chunks (plus the tree nodes needed to verify them) are read,
//! never the whole blob.
//!
//! - [`build_outboard`] streams a blob to compute its outboard and root digest.
//! - [`encode_slice`] (sender) turns a data source + outboard into a
//!   self-verifying slice for a byte range.
//! - [`decode_slice`] (receiver) verifies such a slice against a known root
//!   digest and returns the covered bytes, failing on any mismatch.
//!
//! The bao root hash equals `BLAKE3(content)` for any block size, so a blob's
//! [`BlobId`] wraps its bao root.

use std::io;

use bao_tree::io::fsm::{CreateOutboard, decode_ranges, encode_ranges_validated};
use bao_tree::io::outboard::PreOrderOutboard;
use bao_tree::io::round_up_to_chunks;
use bao_tree::{BaoTree, BlockSize, ByteRanges};
use bytes::{Bytes, BytesMut};
use iroh_io::AsyncSliceReader;

use crate::digest::BlobId;
use crate::error::Error;

pub(crate) mod ingest;
pub(crate) mod patch;
pub(crate) mod stream;

/// bao block size: 16 KiB (2^4 BLAKE3 chunks). Coarser blocks mean smaller
/// outboards and coarser verification granularity; the root hash is unaffected.
pub(crate) const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);

fn to_hash(digest: &BlobId) -> bao_tree::blake3::Hash {
    bao_tree::blake3::Hash::from(*digest.digest().as_bytes())
}

// Slice proofs can include parents below the retained 16 KiB group level.
// Accept and discard those already-verified nodes as well; bao-tree's
// EmptyOutboard rejects saves outside its retained geometry.
struct DiscardOutboard {
    root: bao_tree::blake3::Hash,
    tree: BaoTree,
}

impl bao_tree::io::fsm::Outboard for DiscardOutboard {
    fn root(&self) -> bao_tree::blake3::Hash {
        self.root
    }
    fn tree(&self) -> BaoTree {
        self.tree
    }
    async fn load(
        &mut self,
        _: bao_tree::TreeNode,
    ) -> io::Result<Option<(bao_tree::blake3::Hash, bao_tree::blake3::Hash)>> {
        Ok(None)
    }
}

impl bao_tree::io::fsm::OutboardMut for DiscardOutboard {
    async fn save(
        &mut self,
        _: bao_tree::TreeNode,
        _: &(bao_tree::blake3::Hash, bao_tree::blake3::Hash),
    ) -> io::Result<()> {
        Ok(())
    }
    async fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The `[offset, offset + len)` range a slice covers within a blob of `size`
/// bytes.
///
/// The single place the range rule lives: `offset + len` must not overflow
/// (rather than panicking in debug or wrapping in release), and a slice can only
/// cover bytes the blob actually has. An empty range is valid at any in-range
/// offset; callers short-circuit on it rather than handing it to bao-tree, which
/// rejects empty range sets.
pub(crate) fn covered(offset: u64, len: u64, size: u64) -> io::Result<std::ops::Range<u64>> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| io::Error::other("slice range offset + len overflows u64"))?;
    if end > size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("slice range {offset}..{end} exceeds blob size {size}"),
        ));
    }
    Ok(offset..end)
}

/// Stream `data` to compute its bao outboard and root [`BlobId`].
pub async fn build_outboard<D: AsyncSliceReader>(data: D) -> Result<(Bytes, BlobId), Error> {
    let outboard = PreOrderOutboard::<BytesMut>::create(data, BLOCK_SIZE).await?;
    let root = BlobId::new((*outboard.root.as_bytes()).into());
    Ok((outboard.data.freeze(), root))
}

/// Produce a self-verifying slice covering `[offset, offset + len)` by streaming
/// the covered chunks from `data`, using the blob's `outboard`, `digest`, and
/// `size`. Fails if the range extends beyond the blob.
pub async fn encode_slice<D: AsyncSliceReader>(
    data: D,
    outboard: Bytes,
    digest: &BlobId,
    size: u64,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, Error> {
    let range = covered(offset, len, size)?;
    // an empty range encodes to nothing; bao-tree rejects empty range sets, and
    // decode_slice short-circuits the same case.
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let outboard = PreOrderOutboard {
        root: to_hash(digest),
        tree: BaoTree::new(size, BLOCK_SIZE),
        data: outboard,
    };
    let ranges = round_up_to_chunks(&ByteRanges::from(range));
    let mut encoded = Vec::new();
    encode_ranges_validated(data, outboard, &ranges, &mut encoded)
        .await
        .map_err(io::Error::other)?;
    Ok(encoded)
}

/// Verify a slice against `digest` and `size`, returning the covered
/// `[offset, offset + len)` bytes. Fails on any hash mismatch, or if the range
/// extends beyond the blob.
pub async fn decode_slice(
    slice: &[u8],
    digest: &BlobId,
    size: u64,
    offset: u64,
    len: u64,
) -> Result<Bytes, Error> {
    let range = covered(offset, len, size)?;
    // an empty range is trivially satisfied; decoding it would leave the window
    // base unset and then fail the length check below.
    if range.is_empty() {
        return Ok(Bytes::new());
    }
    let mut outboard = DiscardOutboard {
        root: to_hash(digest),
        tree: BaoTree::new(size, BLOCK_SIZE),
    };
    let ranges = round_up_to_chunks(&ByteRanges::from(range));
    let mut target = WindowWriter::default();
    decode_ranges(slice, ranges, &mut target, &mut outboard)
        .await
        .map_err(io::Error::other)?;

    // decoded blocks start at the covered range's block-aligned base, not 0.
    let start = offset
        .checked_sub(target.base.unwrap_or(0))
        .ok_or_else(|| io::Error::other("decoded slice starts past the requested offset"))?
        as usize;
    let end = start + len as usize;
    if target.buf.len() < end {
        return Err(io::Error::other("decoded slice shorter than requested").into());
    }
    Ok(target.buf.freeze().slice(start..end))
}

/// An [`iroh_io::AsyncSliceWriter`] that stores writes relative to the first
/// write's offset. Decoding a slice writes decoded blocks at their absolute
/// blob offsets; anchoring the buffer at the first write avoids allocating
/// (and zero-filling) everything before the covered range.
#[derive(Default)]
struct WindowWriter {
    /// Absolute offset the buffer starts at (set by the first write).
    base: Option<u64>,
    buf: BytesMut,
}

impl iroh_io::AsyncSliceWriter for WindowWriter {
    async fn write_bytes_at(&mut self, offset: u64, data: Bytes) -> io::Result<()> {
        self.write_at(offset, &data).await
    }

    async fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let base = *self.base.get_or_insert(offset);
        let start = offset
            .checked_sub(base)
            .ok_or_else(|| io::Error::other("write before the decode window base"))?
            as usize;
        let end = start + data.len();
        if self.buf.len() < end {
            self.buf.resize(end, 0);
        }
        self.buf[start..end].copy_from_slice(data);
        Ok(())
    }

    async fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn set_len(&mut self, _len: u64) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slice_roundtrip() {
        let data: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
        let bytes = Bytes::from(data.clone());
        let size = data.len() as u64;

        let (outboard, digest) = build_outboard(bytes.clone()).await.unwrap();
        assert_eq!(digest, BlobId::new(blake3::hash(&data).into()));

        let (offset, len) = (12_345u64, 5000u64);
        let slice = encode_slice(bytes, outboard, &digest, size, offset, len)
            .await
            .unwrap();
        let verified = decode_slice(&slice, &digest, size, offset, len)
            .await
            .unwrap();
        assert_eq!(verified, &data[offset as usize..(offset + len) as usize]);
    }

    #[tokio::test]
    async fn tampering_is_detected() {
        let data: Vec<u8> = (0..40_000u32).map(|i| i as u8).collect();
        let bytes = Bytes::from(data.clone());
        let size = data.len() as u64;
        let (outboard, digest) = build_outboard(bytes.clone()).await.unwrap();

        let (offset, len) = (0u64, 20_000u64);
        let slice = encode_slice(bytes, outboard, &digest, size, offset, len)
            .await
            .unwrap();

        let mut tampered = slice.clone();
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0xff;
        assert!(
            decode_slice(&tampered, &digest, size, offset, len)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn wrong_digest_is_rejected() {
        let data: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let bytes = Bytes::from(data.clone());
        let size = data.len() as u64;
        let (outboard, digest) = build_outboard(bytes.clone()).await.unwrap();
        let slice = encode_slice(bytes, outboard, &digest, size, 0, 1000)
            .await
            .unwrap();

        let wrong = BlobId::new(blake3::hash(b"not this blob").into());
        assert!(decode_slice(&slice, &wrong, size, 0, 1000).await.is_err());
    }

    #[tokio::test]
    async fn zero_length_slice_is_empty() {
        let data = vec![7u8; 1000];
        let bytes = Bytes::from(data.clone());
        let size = data.len() as u64;
        let (outboard, digest) = build_outboard(bytes.clone()).await.unwrap();

        // an empty range at a non-zero offset decodes to empty, not an error.
        let slice = encode_slice(bytes, outboard, &digest, size, 500, 0)
            .await
            .unwrap();
        assert!(
            decode_slice(&slice, &digest, size, 500, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn overflowing_range_errors() {
        let data = vec![1u8; 100];
        let bytes = Bytes::from(data.clone());
        let (outboard, digest) = build_outboard(bytes.clone()).await.unwrap();
        assert!(
            encode_slice(bytes, outboard, &digest, 100, u64::MAX, 1)
                .await
                .is_err()
        );
    }
}
