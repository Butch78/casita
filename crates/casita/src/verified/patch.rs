//! Independently verify an old range proof and derive an ordinary BLAKE3 ID
//! after a same-length overwrite. Unchanged subtrees are authenticated by the
//! old root; only touched blocks and their ancestors are rehashed.

use crate::{BlobId, error::Error};
use bao_tree::{
    BaoTree, TreeNode,
    io::fsm::{Outboard, OutboardMut},
};
use blake3::hazmat::{Mode, merge_subtrees_non_root, merge_subtrees_root};
use bytes::Bytes;
use std::{collections::BTreeMap, io};

pub(crate) fn aligned(size: u64, offset: u64, len: u64) -> io::Result<std::ops::Range<u64>> {
    let range = super::covered(offset, len, size)?;
    if range.is_empty() {
        return Ok(range);
    }
    let block = super::ingest::BLOCK_BYTES as u64;
    let end = ((range.end - 1) / block * block)
        .saturating_add(block)
        .min(size);
    Ok(offset / block * block..end)
}

type HashPair = (bao_tree::blake3::Hash, bao_tree::blake3::Hash);

struct SparseOutboard {
    tree: BaoTree,
    root: bao_tree::blake3::Hash,
    nodes: BTreeMap<u64, (TreeNode, HashPair)>,
}

impl Outboard for SparseOutboard {
    fn tree(&self) -> BaoTree {
        self.tree
    }
    fn root(&self) -> bao_tree::blake3::Hash {
        self.root
    }
    async fn load(
        &mut self,
        node: TreeNode,
    ) -> io::Result<Option<(bao_tree::blake3::Hash, bao_tree::blake3::Hash)>> {
        Ok(self
            .nodes
            .get(&node.mid().to_bytes())
            .map(|(_, pair)| *pair))
    }
}
impl OutboardMut for SparseOutboard {
    async fn save(
        &mut self,
        node: TreeNode,
        pair: &(bao_tree::blake3::Hash, bao_tree::blake3::Hash),
    ) -> io::Result<()> {
        self.nodes.insert(node.mid().to_bytes(), (node, *pair));
        Ok(())
    }
    async fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct Patched {
    pub digest: BlobId,
    pub nodes: Vec<(u64, [u8; 64])>,
}

pub(crate) async fn verify(
    proof: &[u8],
    old: BlobId,
    size: u64,
    offset: u64,
    replacement: &[u8],
) -> Result<Patched, Error> {
    let range = aligned(size, offset, replacement.len() as u64)?;
    if replacement.is_empty() {
        return Ok(Patched {
            digest: old,
            nodes: Vec::new(),
        });
    }
    let mut outboard = SparseOutboard {
        tree: BaoTree::new(size, super::BLOCK_SIZE),
        root: super::to_hash(&old),
        nodes: BTreeMap::new(),
    };
    let mut target = super::WindowWriter::default();
    let mut input = proof;
    bao_tree::io::fsm::decode_ranges(
        &mut input,
        super::round_up_to_chunks(&bao_tree::ByteRanges::from(range.clone())),
        &mut target,
        &mut outboard,
    )
    .await
    .map_err(io::Error::other)?;
    if !input.is_empty()
        || target.base != Some(range.start)
        || target.buf.len() as u64 != range.end - range.start
    {
        return Err(io::Error::other("invalid overwrite proof extent").into());
    }
    let from = (offset - range.start) as usize;
    target.buf[from..from + replacement.len()].copy_from_slice(replacement);
    let mut nodes = Vec::new();
    let digest = rebuild(
        &outboard,
        0,
        size,
        range.start,
        &target.buf.freeze(),
        true,
        &mut nodes,
    )?;
    Ok(Patched {
        digest: BlobId::new(digest.into()),
        nodes,
    })
}

fn rebuild(
    outboard: &SparseOutboard,
    start: u64,
    size: u64,
    base: u64,
    data: &Bytes,
    root: bool,
    nodes: &mut Vec<(u64, [u8; 64])>,
) -> io::Result<[u8; 32]> {
    if size <= super::ingest::BLOCK_BYTES as u64 {
        let from = usize::try_from(start - base).map_err(io::Error::other)?;
        let bytes = &data[from..from + size as usize];
        return Ok(if root {
            *blake3::hash(bytes).as_bytes()
        } else {
            super::ingest::subtree(bytes, start)
        });
    }
    let left_size = blake3::hazmat::left_subtree_len(size);
    let mid = start + left_size;
    let (node, (left, right)) = outboard
        .nodes
        .get(&mid)
        .ok_or_else(|| io::Error::other("missing overwrite proof parent"))?;
    let end = base + data.len() as u64;
    let left = if base < mid && end > start {
        rebuild(outboard, start, left_size, base, data, false, nodes)?
    } else {
        *left.as_bytes()
    };
    let right = if base < start + size && end > mid {
        rebuild(outboard, mid, size - left_size, base, data, false, nodes)?
    } else {
        *right.as_bytes()
    };
    let mut pair = [0; 64];
    pair[..32].copy_from_slice(&left);
    pair[32..].copy_from_slice(&right);
    let position = outboard
        .tree
        .pre_order_offset(*node)
        .ok_or_else(|| io::Error::other("invalid overwrite parent"))?;
    nodes.push((position * 64, pair));
    Ok(if root {
        *merge_subtrees_root(&left, &right, Mode::Hash).as_bytes()
    } else {
        merge_subtrees_non_root(&left, &right, Mode::Hash)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn overwrite_matches_full_rehash_and_reference_outboard() {
        for size in [
            1, 1023, 1024, 1025, 16383, 16384, 16385, 32768, 32769, 49152, 65537, 524289,
        ] {
            let original = (0..size).map(|i| (i * 13) as u8).collect::<Vec<_>>();
            let (outboard, old) = super::super::build_outboard(Bytes::from(original.clone()))
                .await
                .unwrap();
            for offset in [0, size / 2, size - 1, 16380.min(size - 1)] {
                let count = (size - offset).min(300);
                let replacement = vec![42; count];
                let range = aligned(size as u64, offset as u64, count as u64).unwrap();
                let proof = super::super::encode_slice(
                    Bytes::from(original.clone()),
                    outboard.clone(),
                    &old,
                    size as u64,
                    range.start,
                    range.end - range.start,
                )
                .await
                .unwrap();
                let update = verify(&proof, old, size as u64, offset as u64, &replacement)
                    .await
                    .unwrap();
                let mut changed = original.clone();
                changed[offset..offset + count].copy_from_slice(&replacement);
                assert_eq!(
                    update.digest,
                    BlobId::new(blake3::hash(&changed).into()),
                    "{size}/{offset}"
                );
                let mut patched_outboard = outboard.to_vec();
                for (at, pair) in update.nodes {
                    patched_outboard[at as usize..at as usize + 64].copy_from_slice(&pair);
                }
                let (expected, _) = super::super::build_outboard(Bytes::from(changed))
                    .await
                    .unwrap();
                assert_eq!(patched_outboard, expected, "{size}/{offset}");
                let mut corrupt = proof;
                if !corrupt.is_empty() {
                    corrupt[0] ^= 1;
                    assert!(
                        verify(&corrupt, old, size as u64, offset as u64, &replacement)
                            .await
                            .is_err()
                    );
                }
            }
        }
    }
}
