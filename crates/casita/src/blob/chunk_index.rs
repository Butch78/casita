//! A cheap in-memory presence cache for chunk digests.
//!
//! FastCDC deduplication needs a "do I already have this chunk?" check per
//! chunk. On an object store, doing a `HEAD` per chunk throttles writes. The
//! [`ChunkIndex`] remembers chunks known to be present so repeat chunks (within
//! a process, across many blobs) skip the round-trip entirely. It is an
//! optimization for the warm case: a cold index just falls back to a `HEAD`. It
//! must never claim a chunk is present after it is gone, though, or a later
//! write would skip re-uploading a chunk that no longer exists; deletions
//! therefore evict it (see [`ChunkIndex::remove`]). A persistent (e.g.
//! SQLite-backed) index can replace this later without touching callers.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::digest::ChunkId;

/// An in-memory set of chunk digests known to be present in the store.
#[derive(Clone, Default)]
pub struct ChunkIndex {
    known: Arc<RwLock<HashSet<ChunkId>>>,
}

impl ChunkIndex {
    /// Whether this chunk is known to be present.
    pub(crate) fn contains(&self, digest: &ChunkId) -> bool {
        self.known.read().unwrap().contains(digest)
    }

    /// Record that this chunk is present.
    pub(crate) fn insert(&self, digest: ChunkId) {
        self.known.write().unwrap().insert(digest);
    }

    /// Forget this chunk (e.g. after it is deleted from the store), so a later
    /// write does not dedup against a cached entry and skip re-uploading a
    /// chunk that is no longer on disk.
    pub(crate) fn remove(&self, digest: &ChunkId) {
        self.known.write().unwrap().remove(digest);
    }
}
