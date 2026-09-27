//! Derive an ordinary BLAKE3 root and Bao outboard in one input pass.
//! Intermediate metadata spills after 64 KiB; plaintext feeds a streaming hasher.

use crate::BlobId;
use blake3::hazmat::{HasherExt, Mode, merge_subtrees_non_root, merge_subtrees_root};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(crate) const BLOCK_BYTES: usize = 16 * 1024;
const MEMORY_BYTES: usize = 64 * 1024;

pub(crate) struct OutboardData {
    pub file: tempfile::SpooledTempFile,
    pub len: u64,
}

pub(crate) struct IngestHasher {
    leaves: tempfile::SpooledTempFile,
    count: u64,
    current: blake3::Hasher,
    current_len: usize,
}

impl Default for IngestHasher {
    fn default() -> Self {
        Self {
            leaves: tempfile::spooled_tempfile(MEMORY_BYTES),
            count: 0,
            current: blake3::Hasher::new(),
            current_len: 0,
        }
    }
}

pub(crate) fn subtree(bytes: &[u8], offset: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.set_input_offset(offset).update(bytes);
    hasher.finalize_non_root()
}

impl IngestHasher {
    fn push_leaf(&mut self) -> io::Result<()> {
        self.leaves.write_all(&self.current.finalize_non_root())?;
        self.count += 1;
        self.current_len = 0;
        Ok(())
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.current_len == BLOCK_BYTES {
                self.push_leaf()?;
                let offset = self
                    .count
                    .checked_mul(BLOCK_BYTES as u64)
                    .ok_or_else(|| io::Error::other("BLAKE3 input exceeds size limit"))?;
                self.current = blake3::Hasher::new();
                self.current.set_input_offset(offset);
            }
            let count = bytes.len().min(BLOCK_BYTES - self.current_len);
            self.current.update(&bytes[..count]);
            self.current_len += count;
            bytes = &bytes[count..];
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<(BlobId, OutboardData)> {
        let mut output = tempfile::spooled_tempfile(MEMORY_BYTES);
        if self.count == 0 {
            return Ok((
                BlobId::new(self.current.finalize().into()),
                OutboardData {
                    file: output,
                    len: 0,
                },
            ));
        }
        self.push_leaf()?;
        self.leaves.rewind()?;
        let root = parents(&mut self.leaves, self.count, &mut output, 0, true)?;
        output.rewind()?;
        Ok((
            BlobId::new(root.into()),
            OutboardData {
                file: output,
                len: (self.count - 1) * 64,
            },
        ))
    }
}

fn parents(
    leaves: &mut impl Read,
    count: u64,
    output: &mut tempfile::SpooledTempFile,
    position: u64,
    root: bool,
) -> io::Result<[u8; 32]> {
    if count == 1 {
        let mut leaf = [0; 32];
        leaves.read_exact(&mut leaf)?;
        return Ok(leaf);
    }
    let split = 1u64 << (u64::BITS - (count - 1).leading_zeros() - 1);
    let left = parents(leaves, split, output, position + 64, false)?;
    let right = parents(leaves, count - split, output, position + split * 64, false)?;
    output.seek(SeekFrom::Start(position))?;
    output.write_all(&left)?;
    output.write_all(&right)?;
    Ok(if root {
        *merge_subtrees_root(&left, &right, Mode::Hash).as_bytes()
    } else {
        merge_subtrees_non_root(&left, &right, Mode::Hash)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[tokio::test]
    async fn ordinary_hash_and_reference_outboard_at_boundaries() {
        for size in [
            0, 1, 1023, 1024, 1025, 16383, 16384, 16385, 32768, 32769, 49152, 65535, 65536, 65537,
            262145,
        ] {
            let data = Bytes::from((0..size).map(|i| (i * 17) as u8).collect::<Vec<_>>());
            let mut hasher = IngestHasher::default();
            for piece in data.chunks(777) {
                hasher.update(piece).unwrap();
            }
            let (root, mut outboard) = hasher.finish().unwrap();
            let mut encoded = Vec::new();
            outboard.file.read_to_end(&mut encoded).unwrap();
            assert_eq!(root, BlobId::new(blake3::hash(&data).into()), "size {size}");
            let (reference, reference_root) = crate::verified::build_outboard(data).await.unwrap();
            assert_eq!(root, reference_root);
            assert_eq!(encoded, reference, "size {size}");
        }
    }
}
