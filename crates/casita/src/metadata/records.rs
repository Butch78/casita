//! Mutable application records. Values are opaque and never imply GC roots.
use super::{MetadataError, MetadataMutation, RootChange};
use crate::{NamespaceId, ObjectKey, RepositoryRevision, RootName};
use bytes::Bytes;

/// A bytewise key within an application namespace. Empty and non-UTF-8 keys
/// are allowed; `/` has no special meaning to Casita.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetadataKey {
    /// Application namespace, for example `obrador.v1`.
    pub namespace: NamespaceId,
    /// Application-defined key, for example `referrers/target/source`.
    pub key: Bytes,
}

impl MetadataKey {
    /// Construct a key. Operation limits are validated when it is used.
    pub fn new(namespace: NamespaceId, key: impl Into<Bytes>) -> Self {
        Self {
            namespace,
            key: key.into(),
        }
    }
}

/// An opaque application record, independent of the object graph and GC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataRecord {
    /// Namespaced key.
    pub key: MetadataKey,
    /// Uninterpreted application bytes.
    pub value: Bytes,
}

/// Compare a current value, not its history. `None` means expected absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataCheck {
    /// Compare an application record.
    Record {
        key: MetadataKey,
        expected: Option<Bytes>,
    },
    /// Compare a durable GC root.
    Root {
        name: RootName,
        expected: Option<ObjectKey>,
    },
}

/// One change in an atomic metadata commit. Duplicate change keys are rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataChange {
    /// Insert or replace opaque bytes. Does not protect any object from GC.
    Set { key: MetadataKey, value: Bytes },
    /// Delete an application record if present.
    Delete { key: MetadataKey },
    /// Protect a complete, verified object graph with a durable name.
    SetRoot { name: RootName, target: ObjectKey },
    /// Remove a durable GC root if present.
    RemoveRoot { name: RootName },
}

/// Outcome of an atomic commit. A conflict makes no changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataCommitResult {
    /// All changes became visible at this revision.
    Committed { revision: RepositoryRevision },
    /// Index of the first mismatching check in the caller's input order.
    Conflict { check_index: usize },
}

/// A continuation bound to a prefix and a snapshot revision. Keep a
/// `MetadataReader` to paginate while other writers commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataCursor {
    pub(crate) revision: RepositoryRevision,
    pub(crate) prefix: MetadataKey,
    pub(crate) after: Bytes,
}

/// One ordered, bounded prefix range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataPage {
    /// Records in ascending bytewise key order.
    pub records: Vec<MetadataRecord>,
    /// Continuation, present only when at least one more record exists.
    pub cursor: Option<MetadataCursor>,
}

pub(crate) const MAX_KEY_BYTES: usize = 4096;
pub(crate) const MAX_VALUE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_BATCH: usize = 4096;
pub(crate) const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_PAGE: usize = 1024;

pub(crate) fn invalid(message: &str) -> MetadataError {
    MetadataError::InvalidMetadata(message.into())
}

pub(crate) fn validate_key(key: &MetadataKey) -> Result<(), MetadataError> {
    if key.key.len() > MAX_KEY_BYTES {
        return Err(invalid("metadata key exceeds 4096 bytes"));
    }
    Ok(())
}

pub(crate) fn validate_get(keys: &[MetadataKey]) -> Result<(), MetadataError> {
    if keys.len() > MAX_BATCH {
        return Err(invalid("metadata get exceeds 4096 keys"));
    }
    keys.iter().try_for_each(validate_key)
}

pub(crate) fn validate_scan(
    prefix: &MetadataKey,
    after: Option<&[u8]>,
    limit: usize,
) -> Result<(), MetadataError> {
    validate_key(prefix)?;
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("metadata scan limit must be 1..=1024"));
    }
    if after.is_some_and(|key| key.len() > MAX_KEY_BYTES || !key.starts_with(&prefix.key)) {
        return Err(invalid("metadata cursor is outside its prefix"));
    }
    Ok(())
}

/// Exclusive upper bound for a raw byte prefix. `None` covers the empty prefix
/// and all-0xff prefixes; the namespace remains constrained in either case.
pub(crate) fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last != 255 {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

impl MetadataMutation {
    /// Build checked record/root changes for an atomic logical commit.
    /// Limits: 4096 checks/changes, 4 KiB keys, 1 MiB values, 16 MiB input bytes.
    pub fn with_metadata(
        checks: Vec<MetadataCheck>,
        changes: Vec<MetadataChange>,
    ) -> Result<Self, MetadataError> {
        if checks.len() > MAX_BATCH || changes.len() > MAX_BATCH {
            return Err(invalid("metadata commit exceeds 4096 checks or changes"));
        }
        let mut bytes = 0;
        let mut account = |key: &MetadataKey, value: Option<&Bytes>| -> Result<(), MetadataError> {
            validate_key(key)?;
            if value.is_some_and(|v| v.len() > MAX_VALUE_BYTES) {
                return Err(invalid("metadata value exceeds 1 MiB"));
            }
            bytes += key.key.len() + value.map_or(0, Bytes::len);
            if bytes > MAX_BATCH_BYTES {
                return Err(invalid("metadata commit exceeds 16 MiB"));
            }
            Ok(())
        };
        for check in &checks {
            if let MetadataCheck::Record { key, expected } = check {
                account(key, expected.as_ref())?;
            }
        }
        let mut mutation = Self::new();
        mutation.checks = checks;
        for change in changes {
            match change {
                MetadataChange::Set { key, value } => {
                    account(&key, Some(&value))?;
                    if mutation.records.insert(key, Some(value)).is_some() {
                        return Err(invalid("duplicate metadata change"));
                    }
                }
                MetadataChange::Delete { key } => {
                    account(&key, None)?;
                    if mutation.records.insert(key, None).is_some() {
                        return Err(invalid("duplicate metadata change"));
                    }
                }
                MetadataChange::SetRoot { name, target } => {
                    if mutation
                        .roots
                        .insert(name.clone(), RootChange::Set { name, target })
                        .is_some()
                    {
                        return Err(invalid("duplicate root change"));
                    }
                }
                MetadataChange::RemoveRoot { name } => {
                    if mutation
                        .roots
                        .insert(name.clone(), RootChange::Remove { name })
                        .is_some()
                    {
                        return Err(invalid("duplicate root change"));
                    }
                }
            }
        }
        Ok(mutation)
    }

    pub(crate) fn has_metadata(&self) -> bool {
        !self.records.is_empty() || !self.checks.is_empty()
    }

    /// A collection commit replaces the retained object set and nothing else;
    /// combining it with inserts, root changes or records is rejected.
    pub(crate) fn reject_mixed_collection(&self) -> Result<(), super::MetadataError> {
        if self.retained_objects.is_some()
            && (!self.objects.is_empty() || !self.roots.is_empty() || self.has_metadata())
        {
            return Err(super::MetadataError::MixedCollectionMutation);
        }
        Ok(())
    }
}

// Result memory is bounded independently of the number of records. A scan
// fetches one lookahead row past this budget to create an exact continuation.
pub(crate) fn page_len(records: &[MetadataRecord], limit: usize) -> usize {
    let mut bytes = 0;
    records
        .iter()
        .take(limit)
        .take_while(|record| {
            bytes += record.key.key.len() + record.value.len();
            bytes <= MAX_BATCH_BYTES
        })
        .count()
}
