//! Immutable-pack tuning shared by storage and repository constructors.

use super::{
    DEFAULT_LOCAL_PACK_TARGET_SIZE, DEFAULT_PACK_CACHE_CAPACITY, DEFAULT_PACK_TARGET_SIZE,
};

/// Storage tuning for immutable packs. This does not change their format or
/// durability guarantees. `Default` uses the remote-storage pack target;
/// [`Self::local`] selects the smaller local-storage target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackOptions {
    /// Approximate compressed bytes per pack. A pack may exceed this by one chunk.
    pub target_size: u64,
    /// Maximum cached compressed-chunk bytes per handle. Zero disables caching;
    /// reads still coalesce nearby ranges.
    pub cache_capacity: u64,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            target_size: DEFAULT_PACK_TARGET_SIZE,
            cache_capacity: DEFAULT_PACK_CACHE_CAPACITY,
        }
    }
}

impl PackOptions {
    /// The local-storage defaults: smaller packs and the standard bounded cache.
    pub fn local() -> Self {
        Self {
            target_size: DEFAULT_LOCAL_PACK_TARGET_SIZE,
            ..Self::default()
        }
    }
}
