use crate::DirectorySnapshot;
use std::collections::VecDeque;

const CAPACITY: usize = 128;

/// Bounded transport-owned snapshots. A missing verifier means restart required.
#[derive(Default)]
pub(crate) struct Enumerations {
    next: u64,
    snapshots: VecDeque<(u64, u64, bool, DirectorySnapshot)>,
}

impl Enumerations {
    pub fn insert(
        &mut self,
        parent: u64,
        attributes: bool,
        entries: DirectorySnapshot,
    ) -> std::io::Result<u64> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("directory verifier exhausted"))?;
        if self.snapshots.len() == CAPACITY {
            self.snapshots.pop_front();
        }
        self.snapshots
            .push_back((self.next, parent, attributes, entries));
        Ok(self.next)
    }

    pub fn get(&self, verifier: u64, parent: u64, attributes: bool) -> Option<DirectorySnapshot> {
        self.snapshots
            .iter()
            .find(|(v, p, a, _)| *v == verifier && *p == parent && *a == attributes)
            .map(|(_, _, _, entries)| entries.clone())
    }

    pub fn clear(&mut self) {
        self.snapshots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirectoryEntry, FileKind};

    fn snapshot(name: &[u8]) -> DirectorySnapshot {
        DirectorySnapshot(
            vec![DirectoryEntry {
                name: name.to_vec(),
                id: 7,
                kind: FileKind::File,
                metadata: None,
            }]
            .into(),
        )
    }

    #[test]
    fn retained_verifiers_keep_names_and_reject_other_directories_or_modes() {
        let mut cache = Enumerations::default();
        let old = cache.insert(2, true, snapshot(b"old-\xff")).unwrap();
        let new = cache.insert(2, true, snapshot(b"new")).unwrap();
        assert_ne!(old, new);
        assert_eq!(cache.get(old, 2, true).unwrap().0[0].name, b"old-\xff");
        assert!(cache.get(old, 3, true).is_none());
        assert!(cache.get(old, 2, false).is_none());
        assert!(cache.get(0, 2, true).is_none());
        cache.clear();
        assert!(cache.get(new, 2, true).is_none());
    }

    #[test]
    fn oldest_verifier_expires_only_after_capacity_is_exceeded() {
        let mut cache = Enumerations::default();
        let first = cache.insert(2, false, snapshot(b"first")).unwrap();
        for _ in 1..CAPACITY {
            cache.insert(2, false, snapshot(b"other")).unwrap();
        }
        assert!(cache.get(first, 2, false).is_some());
        let last = cache.insert(2, false, snapshot(b"last")).unwrap();
        assert!(cache.get(first, 2, false).is_none());
        assert!(cache.get(last, 2, false).is_some());
        assert_eq!(cache.snapshots.len(), CAPACITY);
    }
}
