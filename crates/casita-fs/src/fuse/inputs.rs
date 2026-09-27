//! Leased roots for a shared read-only input mount. Root names are never
//! reused, and each binding is immutable until its lease is dropped. The
//! complete mount is a host-side implementation detail, never a sandbox bind.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use casita::Node;

use crate::{FilesystemEntry, FilesystemNode};

#[derive(Default)]
struct State {
    next: u64,
    roots: BTreeMap<Vec<u8>, Node>,
}

/// Host-side registry of immutable input bindings with non-reused names.
#[derive(Default)]
pub struct InputRegistry(Mutex<State>);

impl InputRegistry {
    /// Add exactly these inputs. Original names stay private to the lease;
    /// generated names are safe single filesystem components.
    pub fn register(self: &Arc<Self>, inputs: BTreeMap<Vec<u8>, Node>) -> io::Result<InputLease> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("input registry poisoned"))?;
        let generation = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("input registry exhausted"))?;
        let mut names = BTreeMap::new();
        for (index, (original, node)) in inputs.into_iter().enumerate() {
            let token = format!("{generation}-{index}").into_bytes();
            state.roots.insert(token.clone(), node);
            names.insert(original, token);
        }
        Ok(InputLease {
            registry: self.clone(),
            names,
        })
    }

    pub(super) fn root(&self, name: &[u8]) -> io::Result<Option<FilesystemNode>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| io::Error::other("input registry poisoned"))?
            .roots
            .get(name)
            .cloned()
            .map(FilesystemNode::from_casita))
    }

    pub(super) fn roots(&self) -> io::Result<Vec<FilesystemEntry>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| io::Error::other("input registry poisoned"))?
            .roots
            .iter()
            .map(|(name, node)| FilesystemEntry {
                name: name.clone(),
                node: FilesystemNode::from_casita(node.clone()),
            })
            .collect())
    }
}

/// Holds the input names until the owning sandbox has exited. Existing kernel
/// references remain valid; removing a lease prevents new root lookups only.
pub struct InputLease {
    registry: Arc<InputRegistry>,
    names: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl InputLease {
    /// The mount-relative component for a declared input, if registered.
    pub fn name(&self, original: &[u8]) -> Option<&[u8]> {
        self.names.get(original).map(Vec::as_slice)
    }
}

impl Drop for InputLease {
    fn drop(&mut self) {
        let mut state = self.registry.0.lock().unwrap_or_else(|e| e.into_inner());
        for token in self.names.values() {
            state.roots.remove(token);
        }
    }
}
