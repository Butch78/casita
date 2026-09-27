//! One owner for immutable directory lookups and their enumeration snapshot.
use crate::filesystem::DirectorySnapshot;
use casita_fs::{FilesystemEntry, FilesystemNode};
use std::{collections::BTreeMap, sync::OnceLock};

pub(super) struct CachedDirectory {
    nodes: BTreeMap<Vec<u8>, FilesystemNode>,
    snapshot: OnceLock<DirectorySnapshot>,
}

impl CachedDirectory {
    pub(super) fn new(entries: Vec<FilesystemEntry>) -> Self {
        Self {
            nodes: entries
                .into_iter()
                .map(|entry| (entry.name, entry.node))
                .collect(),
            snapshot: OnceLock::new(),
        }
    }

    pub(super) fn get(&self, name: &[u8]) -> Option<&FilesystemNode> {
        self.nodes.get(name)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&[u8], &FilesystemNode)> {
        self.nodes
            .iter()
            .map(|(name, node)| (name.as_slice(), node))
    }

    /// Only enumeration builds native entries. Lookup misses retain directory
    /// metadata without allocating inodes or a second array of names.
    pub(super) fn snapshot(
        &self,
        retain: bool,
        build: impl FnOnce() -> DirectorySnapshot,
    ) -> DirectorySnapshot {
        if retain {
            self.snapshot.get_or_init(build).clone()
        } else {
            build()
        }
    }
}
