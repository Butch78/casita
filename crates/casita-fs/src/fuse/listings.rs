//! Directory listings, cached across mounts.
//!
//! A listing is keyed by the directory's content address, so an entry is
//! valid for as long as the process lives: there is no such thing as a stale
//! listing when the key *is* the content. Two builds that share a stdenv
//! share the decode of every directory in it, and a nix process that runs a
//! thousand derivations pays for each directory once rather than once per
//! build.
//!
//! Bounded by a two generation scheme rather than an LRU: when the young
//! generation fills, it becomes the old one and a fresh map takes over, so a
//! lookup checks two maps, promotion is a clone of one `Arc`, and eviction is
//! dropping a map. Worst case a still-hot listing is decoded once more per
//! generation, which is far cheaper than the bookkeeping an exact LRU wants.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::{ContentKey, FilesystemEntry};

/// Directory entries, sorted by name.
pub(crate) type Listing = Arc<Vec<FilesystemEntry>>;

/// How many directories one generation holds before it ages out. A listing is
/// names plus one node each, so a few thousand directories is a handful of
/// megabytes, and the closures a builder touches repeatedly are far smaller
/// than that.
const GENERATION_CAPACITY: usize = 4096;

#[derive(Default)]
pub struct ListingCache {
    maps: Mutex<Generations>,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Default)]
struct Generations {
    young: HashMap<ContentKey, Listing>,
    old: HashMap<ContentKey, Listing>,
}

impl ListingCache {
    /// The cache every mount shares unless it is given its own.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<ListingCache>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(ListingCache::default())))
    }

    pub(crate) fn get(&self, key: &ContentKey) -> Option<Listing> {
        let mut maps = self.maps.lock().expect("listing cache poisoned");
        if let Some(listing) = maps.young.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Some(Arc::clone(listing));
        }
        // Surviving a generation earns promotion, so a directory a build keeps
        // returning to does not age out from under it.
        if let Some(listing) = maps.old.get(key).map(Arc::clone) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            maps.young.insert(key.clone(), Arc::clone(&listing));
            return Some(listing);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    pub(crate) fn insert(&self, key: ContentKey, listing: Listing) {
        let mut maps = self.maps.lock().expect("listing cache poisoned");
        if maps.young.len() >= GENERATION_CAPACITY {
            maps.old = std::mem::take(&mut maps.young);
        }
        maps.young.insert(key, listing);
    }

    /// Hits and misses since the process started. Whether a build is finding
    /// what earlier builds decoded is the only question this cache has to
    /// answer.
    #[must_use]
    pub fn counts(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}
