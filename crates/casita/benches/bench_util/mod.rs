//! Shared helpers for casita benchmarks.
//!
//! This module owns deterministic corpus generators plus benchmark-only
//! payload-backend constructors and the Tokio runtime builder.

// each bench binary uses only a subset of the helpers and re-exports below.
#![allow(dead_code, unused_imports)]

pub mod perf;

use std::sync::Arc;

use casita::experimental::{BlobId, BlobStore, ChunkedBlobStore};
use object_store::{memory::InMemory, path::Path};
use tokio::io::AsyncWriteExt;

/// SplitMix64, used only to make repeatable benchmark input.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

/// Repeatable pseudo-random bytes that zstd cannot substantially shrink.
pub fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut output = Vec::with_capacity(len + 8);
    while output.len() < len {
        output.extend_from_slice(&rng.next_u64().to_le_bytes());
    }
    output.truncate(len);
    output
}

/// Repeatable structured bytes that model compressible source content.
pub fn compressible_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut output = Vec::with_capacity(len + 64);
    while output.len() < len {
        let count = rng.next_u64() % 32;
        output.extend_from_slice(
            format!("the quick brown fox jumps {count:02} times over the lazy dog\n").as_bytes(),
        );
    }
    output.truncate(len);
    output
}

/// Repeatable mixed corpus with alternating structured and incompressible
/// regions. This more closely resembles source trees plus generated assets than
/// either homogeneous benchmark input on its own.
pub fn mixed_corpus(seed: u64, len: usize) -> Vec<u8> {
    const BLOCK: usize = 64 * 1024;
    let mut output = Vec::with_capacity(len);
    let mut block = 0u64;
    while output.len() < len {
        let remaining = len - output.len();
        let take = remaining.min(BLOCK);
        let bytes = if block.is_multiple_of(2) {
            compressible_bytes(seed.wrapping_add(block), take)
        } else {
            random_bytes(seed.wrapping_add(block), take)
        };
        output.extend_from_slice(&bytes);
        block += 1;
    }
    output
}

/// Write a payload and assert that its reported size is exact.
pub async fn write_blob(payloads: &impl BlobStore, data: &[u8]) -> BlobId {
    let mut writer = payloads.open_write().await;
    writer.write_all(data).await.unwrap();
    let (id, size) = writer.close().await.unwrap();
    assert_eq!(size, data.len() as u64);
    id
}

/// A current-thread Tokio runtime: enough for the write path, which offloads CPU
/// work to the blocking pool and needs neither the IO driver nor extra threads.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// A [`ChunkedBlobStore`] over a fresh in-memory object store, returned together
/// with a handle to that backend for byte accounting.
pub fn memory_store(avg_chunk_size: u32) -> (ChunkedBlobStore, Arc<InMemory>) {
    let backend = Arc::new(InMemory::new());
    let store = ChunkedBlobStore::new(backend.clone(), Path::default(), avg_chunk_size);
    (store, backend)
}

/// A second [`ChunkedBlobStore`] over an existing backend, with a *cold* chunk
/// index. Reads see the chunks already on disk but every presence check falls
/// back to a `HEAD`, modelling a freshly started process.
pub fn store_over(backend: Arc<InMemory>, avg_chunk_size: u32) -> ChunkedBlobStore {
    ChunkedBlobStore::new(backend, Path::default(), avg_chunk_size)
}

/// A [`ChunkedBlobStore`] over a temp directory on the real filesystem. The
/// [`tempfile::TempDir`] must be kept alive for the store's lifetime.
pub fn local_store(avg_chunk_size: u32) -> (ChunkedBlobStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let backend = object_store::local::LocalFileSystem::new_with_prefix(dir.path()).unwrap();
    let store = ChunkedBlobStore::new(Arc::new(backend), Path::default(), avg_chunk_size);
    (store, dir)
}

pub mod io_counter;
