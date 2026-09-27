//! The root-closure walk every metadata backend runs before naming a graph.
//!
//! Backends differ only in how they look a record up (an in-memory map, a SQL
//! transaction, sharded object storage), so they drive this state machine
//! with their own lookup instead of each carrying a copy of the traversal.

use std::collections::{BTreeSet, VecDeque};

use super::MetadataError;
use crate::object::ObjectKey;

/// Breadth-first validation of the closures of newly named targets.
///
/// Scoped to names this commit points somewhere new: records are immutable,
/// only collection removes them, and unnaming cannot break another name, so a
/// name that survives a commit unchanged is still complete. A key already
/// validated, or validated by this commit, settles its whole subgraph.
pub(crate) struct ClosureWalk<'a> {
    queue: VecDeque<(Option<ObjectKey>, ObjectKey)>,
    seen: BTreeSet<ObjectKey>,
    newly_validated: &'a BTreeSet<ObjectKey>,
    current: Option<(Option<ObjectKey>, ObjectKey)>,
}

impl<'a> ClosureWalk<'a> {
    pub(crate) fn new(named: &[ObjectKey], newly_validated: &'a BTreeSet<ObjectKey>) -> Self {
        Self {
            queue: named.iter().cloned().map(|target| (None, target)).collect(),
            seen: BTreeSet::new(),
            newly_validated,
            current: None,
        }
    }

    /// The next key to look up, or `None` once every closure is complete.
    pub(crate) fn next_key(&mut self) -> Option<ObjectKey> {
        while let Some((from, key)) = self.queue.pop_front() {
            if self.seen.insert(key.clone()) {
                self.current = Some((from, key.clone()));
                return Some(key);
            }
        }
        self.current = None;
        None
    }

    /// Record the lookup of the key last returned by [`Self::next_key`]: its
    /// ordered links and whether a stored witness already validated it.
    pub(crate) fn visit(
        &mut self,
        found: Option<(&[ObjectKey], bool)>,
    ) -> Result<(), MetadataError> {
        let (from, key) = self
            .current
            .take()
            .expect("visit follows a key returned by next_key");
        let Some((links, validated)) = found else {
            return Err(MetadataError::MissingObject { missing: key, from });
        };
        // A validation witness is meaningful only beside a present record;
        // collection removes both together, so no deeper traversal can prove
        // more than the witness already does.
        if validated || self.newly_validated.contains(&key) {
            return Ok(());
        }
        self.queue.extend(
            links
                .iter()
                .filter(|target| !self.seen.contains(*target))
                .cloned()
                .map(|target| (Some(key.clone()), target)),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn key(name: &str) -> ObjectKey {
        ObjectKey::blob(crate::BlobId::new(blake3::hash(name.as_bytes()).into()))
    }

    fn walk(
        graph: &BTreeMap<ObjectKey, (Vec<ObjectKey>, bool)>,
        named: &[ObjectKey],
        newly_validated: &BTreeSet<ObjectKey>,
    ) -> Result<Vec<ObjectKey>, MetadataError> {
        let mut walk = ClosureWalk::new(named, newly_validated);
        let mut visited = Vec::new();
        while let Some(key) = walk.next_key() {
            visited.push(key.clone());
            walk.visit(
                graph
                    .get(&key)
                    .map(|(links, validated)| (links.as_slice(), *validated)),
            )?;
        }
        Ok(visited)
    }

    #[test]
    fn reports_the_missing_link_and_its_parent() {
        let (root, child, missing) = (key("root"), key("child"), key("missing"));
        let graph = BTreeMap::from([
            (root.clone(), (vec![child.clone()], false)),
            (child.clone(), (vec![missing.clone()], false)),
        ]);
        match walk(&graph, &[root], &BTreeSet::new()) {
            Err(MetadataError::MissingObject {
                missing: reported,
                from,
            }) => {
                assert_eq!(reported, missing);
                assert_eq!(from, Some(child));
            }
            other => panic!("expected a missing object, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_named_target_has_no_parent() {
        let target = key("target");
        match walk(
            &BTreeMap::new(),
            std::slice::from_ref(&target),
            &BTreeSet::new(),
        ) {
            Err(MetadataError::MissingObject { missing, from }) => {
                assert_eq!(missing, target);
                assert_eq!(from, None);
            }
            other => panic!("expected a missing object, got {other:?}"),
        }
    }

    #[test]
    fn validated_witnesses_settle_their_subgraph() {
        let (root, stored, fresh, missing) =
            (key("root"), key("stored"), key("fresh"), key("missing"));
        let graph = BTreeMap::from([
            (root.clone(), (vec![stored.clone(), fresh.clone()], false)),
            (stored.clone(), (vec![missing.clone()], true)),
            (fresh.clone(), (vec![missing], false)),
        ]);
        let newly_validated = BTreeSet::from([fresh.clone()]);
        let visited = walk(&graph, std::slice::from_ref(&root), &newly_validated).unwrap();
        assert_eq!(visited, vec![root, stored, fresh]);
    }

    #[test]
    fn shared_descendants_are_visited_once() {
        let (a, b, shared) = (key("a"), key("b"), key("shared"));
        let graph = BTreeMap::from([
            (a.clone(), (vec![shared.clone()], false)),
            (b.clone(), (vec![shared.clone()], false)),
            (shared.clone(), (vec![], false)),
        ]);
        let visited = walk(&graph, &[a.clone(), b.clone(), a.clone()], &BTreeSet::new()).unwrap();
        assert_eq!(visited, vec![a, b, shared]);
    }
}
