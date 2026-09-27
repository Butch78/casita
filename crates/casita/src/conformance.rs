//! Executable contracts for Casita storage backends.
//!
//! Backend authors can run these checks against a fresh store to verify the
//! semantics shared by the in-memory, local, and composed implementations.
//! The checks intentionally exercise behavior rather than implementation
//! details, so the same battery also applies to future remote adapters.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{Cursor, SeekFrom};

use futures::TryStreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::blob::BlobStore;
use crate::format::{FormatLimits, FormatRegistry, PayloadReader, VerifiedObject};
use crate::metadata::{
    DataPin, MetadataError, MetadataMutation, MetadataSnapshot, MetadataStore, PinResource,
    PinScope, PinStore,
};
use crate::object::{ObjectKey, RootName};
use crate::{BlobId, Digest};

/// A backend operation failed or its externally visible behavior violated the
/// corresponding Casita trait contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ConformanceError {
    message: String,
}

impl ConformanceError {
    fn operation(phase: &str, error: impl fmt::Display) -> Self {
        Self {
            message: format!("backend failed while {phase}: {error}"),
        }
    }

    fn violation(message: impl Into<String>) -> Self {
        Self {
            message: format!("backend contract violation: {}", message.into()),
        }
    }
}

fn require(condition: bool, message: impl Into<String>) -> Result<(), ConformanceError> {
    if condition {
        Ok(())
    } else {
        Err(ConformanceError::violation(message))
    }
}

/// Check the complete generic [`BlobStore`] contract against a fresh store.
pub async fn check_blob_store(store: &dyn BlobStore) -> Result<(), ConformanceError> {
    let absent = BlobId::new(Digest::hash(b"casita-conformance-absent"));
    require(
        !store
            .has(&absent)
            .await
            .map_err(|error| ConformanceError::operation("probing an absent blob", error))?,
        "has returned true for an absent blob",
    )?;
    require(
        store
            .open_read(&absent)
            .await
            .map_err(|error| ConformanceError::operation("opening an absent blob", error))?
            .is_none(),
        "open_read returned a reader for an absent blob",
    )?;
    require(
        store
            .open_stream(&absent)
            .await
            .map_err(|error| ConformanceError::operation("streaming an absent blob", error))?
            .is_none(),
        "open_stream returned a reader for an absent blob",
    )?;
    require(
        store
            .chunks(&absent)
            .await
            .map_err(|error| ConformanceError::operation("querying absent chunks", error))?
            .is_none(),
        "chunks returned metadata for an absent blob",
    )?;

    let payload = b"0123456789-casita-storage-conformance";
    let expected = BlobId::new(Digest::hash(payload));
    let mut writer = store.open_write().await;
    writer
        .write_all(payload)
        .await
        .map_err(|error| ConformanceError::operation("writing a blob", error))?;
    let first_close = writer
        .close()
        .await
        .map_err(|error| ConformanceError::operation("closing a blob", error))?;
    let second_close = writer
        .close()
        .await
        .map_err(|error| ConformanceError::operation("closing a blob twice", error))?;
    require(
        first_close == second_close,
        "BlobWriter::close is not idempotent",
    )?;
    require(
        first_close == (expected, payload.len() as u64),
        "writer returned the wrong whole-plaintext digest or size",
    )?;

    let batch = store
        .has_batch(&[absent, expected, absent, expected])
        .await
        .map_err(|error| ConformanceError::operation("batching presence probes", error))?;
    require(
        batch == [false, true, false, true],
        "has_batch did not preserve input order or duplicates",
    )?;
    require(
        store
            .has_batch(&[])
            .await
            .map_err(|error| ConformanceError::operation("probing an empty batch", error))?
            .is_empty(),
        "has_batch returned entries for an empty input",
    )?;

    let mut reader = store
        .open_read(&expected)
        .await
        .map_err(|error| ConformanceError::operation("opening a stored blob", error))?
        .ok_or_else(|| ConformanceError::violation("a just-written blob is absent"))?;
    reader
        .seek(SeekFrom::Start(10))
        .await
        .map_err(|error| ConformanceError::operation("seeking a blob reader", error))?;
    let mut suffix = Vec::new();
    reader
        .read_to_end(&mut suffix)
        .await
        .map_err(|error| ConformanceError::operation("reading after seek", error))?;
    require(
        suffix == payload[10..],
        "seekable reader returned wrong bytes",
    )?;

    let mut stream = store
        .open_stream(&expected)
        .await
        .map_err(|error| ConformanceError::operation("streaming a stored blob", error))?
        .ok_or_else(|| ConformanceError::violation("a just-written blob cannot be streamed"))?;
    let mut streamed = Vec::new();
    stream
        .read_to_end(&mut streamed)
        .await
        .map_err(|error| ConformanceError::operation("consuming a blob stream", error))?;
    require(
        streamed == payload,
        "sequential stream returned wrong bytes",
    )?;

    let chunks = store
        .chunks(&expected)
        .await
        .map_err(|error| ConformanceError::operation("querying stored chunks", error))?
        .ok_or_else(|| ConformanceError::violation("chunks says a present blob is absent"))?;
    if !chunks.is_empty() {
        let size = chunks
            .iter()
            .try_fold(0u64, |total, chunk| total.checked_add(chunk.size).ok_or(()));
        require(
            size == Ok(payload.len() as u64),
            "chunk sizes do not sum to the blob size",
        )?;
    }

    let duplicate = store
        .put_slice(payload)
        .await
        .map_err(|error| ConformanceError::operation("writing identical bytes twice", error))?;
    require(
        duplicate == expected,
        "identical writes produced different blob identities",
    )
}

/// Check chunk negotiation, verification, and manifest installation between a
/// populated chunk-capable source and a fresh chunk-capable destination.
pub async fn check_blob_sync(
    source: &dyn BlobStore,
    destination: &dyn BlobStore,
) -> Result<(), ConformanceError> {
    let source_sync = source
        .as_blob_sync()
        .ok_or_else(|| ConformanceError::violation("source does not expose BlobSync"))?;
    let destination_sync = destination
        .as_blob_sync()
        .ok_or_else(|| ConformanceError::violation("destination does not expose BlobSync"))?;
    let mut payload = Vec::with_capacity(256 * 1024);
    for index in 0..256 * 1024 {
        payload.push(((index * 31 + index / 97) % 251) as u8);
    }
    let blob = source
        .put_slice(&payload)
        .await
        .map_err(|error| ConformanceError::operation("seeding a sync source", error))?;
    let chunks = source
        .chunks(&blob)
        .await
        .map_err(|error| ConformanceError::operation("reading the source chunk map", error))?
        .ok_or_else(|| ConformanceError::violation("source lost its just-written blob"))?;
    require(
        !chunks.is_empty(),
        "chunk-capable source returned no chunks",
    )?;

    let missing = destination_sync
        .missing_chunks(&chunks)
        .await
        .map_err(|error| ConformanceError::operation("negotiating missing chunks", error))?;
    let unique: BTreeSet<_> = chunks.iter().map(|chunk| chunk.digest).collect();
    require(
        missing.len() == unique.len(),
        "fresh destination did not report every unique chunk missing",
    )?;
    for meta in &missing {
        let compressed = source_sync
            .get_chunk(&meta.digest)
            .await
            .map_err(|error| ConformanceError::operation("reading a source chunk", error))?
            .ok_or_else(|| ConformanceError::violation("advertised source chunk is absent"))?;
        destination_sync
            .put_chunk(meta, compressed)
            .await
            .map_err(|error| ConformanceError::operation("installing a verified chunk", error))?;
    }
    destination_sync
        .put_manifest(&blob, chunks.clone())
        .await
        .map_err(|error| ConformanceError::operation("installing a verified manifest", error))?;
    require(
        destination
            .read_to_vec(&blob)
            .await
            .map_err(|error| ConformanceError::operation("reading a synchronized blob", error))?
            .as_deref()
            == Some(payload.as_slice()),
        "synchronized plaintext differs from the source",
    )?;
    require(
        destination_sync
            .missing_chunks(&chunks)
            .await
            .map_err(|error| ConformanceError::operation("renegotiating chunks", error))?
            .is_empty(),
        "installed chunks are still reported missing",
    )
}

/// Check atomic mutation, snapshot isolation, stale-write rejection,
/// ordering, and collection semantics against a fresh [`MetadataStore`].
pub async fn check_state_store(store: &dyn MetadataStore) -> Result<(), ConformanceError> {
    let initial = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("opening the initial snapshot", error))?;
    require(
        initial
            .objects()
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| ConformanceError::operation("listing initial objects", error))?
            .is_empty(),
        "state conformance requires a fresh store",
    )?;
    require(
        initial
            .roots()
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| ConformanceError::operation("listing initial roots", error))?
            .is_empty(),
        "state conformance requires a fresh store",
    )?;

    let live = verified_blob(b"casita-conformance-live").await?;
    let orphan = verified_blob(b"casita-conformance-orphan").await?;
    let live_key = live.record().key().clone();
    let orphan_key = orphan.record().key().clone();
    let root = RootName::try_from("conformance/live")
        .map_err(|error| ConformanceError::operation("building a root fixture", error))?;
    let mut publish = MetadataMutation::new();
    publish
        .add_objects([live, orphan])
        .set_root(root.clone(), live_key.clone());
    let committed = store
        .commit(&initial.revision(), publish)
        .await
        .map_err(|error| ConformanceError::operation("atomically publishing state", error))?;
    require(
        committed.objects_inserted == 2 && committed.roots_changed == 1,
        "commit counters do not describe the atomic mutation",
    )?;
    require(
        initial
            .object(&live_key)
            .await
            .map_err(|error| ConformanceError::operation("checking snapshot isolation", error))?
            .is_none(),
        "an old snapshot observed a later object",
    )?;

    match store
        .commit(&initial.revision(), MetadataMutation::new())
        .await
    {
        Err(MetadataError::StaleRevision { .. }) => {}
        Err(error) => {
            return Err(ConformanceError::violation(format!(
                "stale commit returned the wrong error: {error}"
            )));
        }
        Ok(_) => return Err(ConformanceError::violation("stale commit succeeded")),
    }

    let retained = BTreeSet::from([live_key.clone()]);
    let collected = store
        .commit(
            &committed.revision,
            MetadataMutation::install_retained_objects(retained),
        )
        .await
        .map_err(|error| ConformanceError::operation("installing a retained set", error))?;
    require(
        collected.objects_removed == 1,
        "collection removed the wrong count",
    )?;
    let after = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("opening collected state", error))?;
    require(
        after
            .root(&root)
            .await
            .map_err(|error| ConformanceError::operation("reading a retained root", error))?
            == Some(live_key.clone()),
        "collection changed a retained root",
    )?;
    require(
        after
            .object(&live_key)
            .await
            .map_err(|error| ConformanceError::operation("reading a retained object", error))?
            .is_some()
            && after
                .object(&orphan_key)
                .await
                .map_err(|error| ConformanceError::operation("reading a removed object", error))?
                .is_none(),
        "collection did not install the exact retained set",
    )?;

    let missing = ObjectKey::blob(BlobId::new(Digest::hash(b"conformance-missing")));
    let mut invalid = MetadataMutation::new();
    invalid.set_root(
        RootName::try_from("conformance/missing")
            .map_err(|error| ConformanceError::operation("building a missing root", error))?,
        missing,
    );
    match store.commit(&after.revision(), invalid).await {
        Err(MetadataError::MissingObject { .. }) => {}
        Err(error) => {
            return Err(ConformanceError::violation(format!(
                "missing root target returned the wrong error: {error}"
            )));
        }
        Ok(_) => {
            return Err(ConformanceError::violation(
                "missing root target was committed",
            ));
        }
    }
    let unchanged = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("checking failed-commit atomicity", error))?;
    require(
        unchanged.revision() == after.revision(),
        "a rejected mutation changed the repository revision",
    )?;
    let objects = unchanged
        .objects()
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| ConformanceError::operation("listing final objects", error))?;
    let roots = unchanged
        .roots()
        .try_collect::<Vec<_>>()
        .await
        .map_err(|error| ConformanceError::operation("listing final roots", error))?;
    require(
        objects.windows(2).all(|pair| pair[0].key() < pair[1].key()),
        "object inventory is not in key order",
    )?;
    require(
        roots.windows(2).all(|pair| pair[0].name() < pair[1].name()),
        "root inventory is not in name order",
    )?;
    check_generations_and_collection_shape(store).await
}

/// Commits advance the snapshot generation, objects keep the generation of
/// their first insertion, and a collection commit changes nothing else.
/// Online retention relies on all three to expand snapshot pins.
async fn check_generations_and_collection_shape(
    store: &dyn MetadataStore,
) -> Result<(), ConformanceError> {
    let generation = |snapshot: &dyn MetadataSnapshot, phase: &str| {
        snapshot
            .generation()
            .map_err(|error| ConformanceError::operation(phase, error))
    };
    let before = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("opening a generation baseline", error))?;
    let base = generation(before.as_ref(), "reading the baseline generation")?;

    let fixture = b"casita-conformance-generation";
    let key = verified_blob(fixture).await?.record().key().clone();
    let mut insert = MetadataMutation::new();
    insert.add_objects([verified_blob(fixture).await?]);
    let inserted = store
        .commit(&before.revision(), insert)
        .await
        .map_err(|error| ConformanceError::operation("inserting a generation fixture", error))?;
    let born = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("opening the inserted state", error))?;
    let birth = generation(born.as_ref(), "reading the insert generation")?;
    require(birth > base, "a commit did not advance the generation")?;
    require(
        !created_through(born.as_ref(), base).await?.contains(&key),
        "an object was born before the commit that inserted it",
    )?;
    require(
        created_through(born.as_ref(), birth).await?.contains(&key),
        "an inserted object is missing from its birth generation",
    )?;

    let mut repeat = MetadataMutation::new();
    repeat.add_objects([verified_blob(fixture).await?]);
    let repeated = store
        .commit(&inserted.revision, repeat)
        .await
        .map_err(|error| ConformanceError::operation("repeating an identical insert", error))?;
    require(
        repeated.objects_inserted == 0,
        "an identical insert was counted as a new object",
    )?;
    let later = store
        .snapshot()
        .await
        .map_err(|error| ConformanceError::operation("opening the repeated state", error))?;
    require(
        generation(later.as_ref(), "reading the repeated generation")? > birth,
        "an idempotent commit did not advance the generation",
    )?;
    require(
        created_through(later.as_ref(), birth).await?.contains(&key),
        "an idempotent insert moved an object's birth generation",
    )?;

    let everything = later
        .objects()
        .map_ok(|record| record.key().clone())
        .try_collect::<BTreeSet<_>>()
        .await
        .map_err(|error| ConformanceError::operation("listing the retained set", error))?;
    let mut mixed = MetadataMutation::install_retained_objects(everything);
    mixed.add_objects([verified_blob(b"casita-conformance-mixed").await?]);
    match store.commit(&later.revision(), mixed).await {
        Err(MetadataError::MixedCollectionMutation) => Ok(()),
        Err(error) => Err(ConformanceError::violation(format!(
            "a mixed collection mutation returned the wrong error: {error}"
        ))),
        Ok(_) => Err(ConformanceError::violation(
            "a collection commit also inserted objects",
        )),
    }
}

async fn created_through(
    snapshot: &dyn MetadataSnapshot,
    generation: u64,
) -> Result<BTreeSet<ObjectKey>, ConformanceError> {
    snapshot
        .objects_created_through(generation)
        .map_ok(|record| record.key().clone())
        .try_collect()
        .await
        .map_err(|error| ConformanceError::operation("listing objects by birth generation", error))
}

/// Check the admission rules online collection relies on against a fresh
/// [`PinStore`]: pinned data cannot be claimed for deletion, deletion claims
/// are exclusive and block conflicting pins, one collector owns a pass, the
/// logical prune fence blocks new payload pins, and a pin released during a
/// pass keeps protecting its data until that pass finishes.
pub async fn check_pin_store(pins: &dyn PinStore) -> Result<(), ConformanceError> {
    let inventory = || async {
        pins.inventory()
            .await
            .map_err(|error| ConformanceError::operation("reading the pin inventory", error))
    };
    let blob = |name: &[u8]| PinResource::Blob(BlobId::new(Digest::hash(name)));
    let staging = |resource: &PinResource| DataPin {
        scope: PinScope::Staging,
        catalog: None,
        resources: BTreeSet::from([resource.clone()]),
    };
    let (held, claimed, disjoint) = (
        blob(b"casita-conformance-pin-held"),
        blob(b"casita-conformance-pin-claimed"),
        blob(b"casita-conformance-pin-disjoint"),
    );

    let fresh = inventory().await?;
    require(
        fresh.pins.is_empty()
            && fresh.deletions.is_empty()
            && fresh.collector.is_none()
            && fresh.logical_prune.is_none(),
        "pin conformance requires a fresh ledger",
    )?;

    let pin = pins
        .register(staging(&held))
        .await
        .map_err(|error| ConformanceError::operation("registering a pin", error))?
        .ok_or_else(|| ConformanceError::violation("an idle ledger refused a pin"))?;
    let current = inventory().await?;
    require(
        current.pins.contains_key(&pin),
        "a registered pin is missing from the inventory",
    )?;
    require(
        pins.claim_deletions(current.revision, BTreeSet::from([held.clone()]))
            .await
            .map_err(|error| ConformanceError::operation("claiming pinned data", error))?
            .is_none(),
        "pinned data was claimed for deletion",
    )?;

    let current = inventory().await?;
    let claim = pins
        .claim_deletions(current.revision, BTreeSet::from([claimed.clone()]))
        .await
        .map_err(|error| ConformanceError::operation("claiming unpinned data", error))?
        .ok_or_else(|| ConformanceError::violation("unpinned data could not be claimed"))?;
    let current = inventory().await?;
    require(
        pins.claim_deletions(current.revision, BTreeSet::from([claimed.clone()]))
            .await
            .map_err(|error| ConformanceError::operation("claiming claimed data", error))?
            .is_none(),
        "two deletion claims own the same data",
    )?;
    require(
        pins.register(staging(&claimed))
            .await
            .map_err(|error| ConformanceError::operation("pinning claimed data", error))?
            .is_none(),
        "a pin was admitted over an active deletion claim",
    )?;
    let beside = pins
        .register(staging(&disjoint))
        .await
        .map_err(|error| ConformanceError::operation("pinning beside a claim", error))?
        .ok_or_else(|| {
            ConformanceError::violation("a disjoint deletion claim blocked an unrelated pin")
        })?;
    pins.finish_deletions(&claim)
        .await
        .map_err(|error| ConformanceError::operation("finishing a deletion claim", error))?;
    require(
        !inventory().await?.deletions.contains_key(&claim),
        "a finished deletion claim remained in the inventory",
    )?;

    let collector = pins
        .acquire_collection(None)
        .await
        .map_err(|error| ConformanceError::operation("acquiring collection", error))?
        .ok_or_else(|| ConformanceError::violation("an idle ledger refused a collector"))?;
    require(
        pins.acquire_collection(None)
            .await
            .map_err(|error| ConformanceError::operation("acquiring a second collector", error))?
            .is_none(),
        "two collectors own the same ledger",
    )?;
    require(
        pins.acquire_collection(Some(pin.clone()))
            .await
            .map_err(|error| ConformanceError::operation("taking over with a stale token", error))?
            .is_none(),
        "a token that never owned collection took it over",
    )?;

    pins.release(&pin)
        .await
        .map_err(|error| ConformanceError::operation("releasing a pin", error))?;
    let current = inventory().await?;
    require(
        pins.claim_deletions(current.revision, BTreeSet::from([held.clone()]))
            .await
            .map_err(|error| ConformanceError::operation("claiming released data", error))?
            .is_none(),
        "a pin released during collection stopped protecting its data",
    )?;

    let current = inventory().await?;
    let fence = pins
        .begin_prune(current.revision)
        .await
        .map_err(|error| ConformanceError::operation("fencing a logical prune", error))?
        .ok_or_else(|| ConformanceError::violation("a collector could not fence its prune"))?;
    require(
        pins.register(staging(&blob(b"casita-conformance-pin-fenced")))
            .await
            .map_err(|error| ConformanceError::operation("pinning during a prune", error))?
            .is_none(),
        "a payload pin was admitted through the logical prune fence",
    )?;
    pins.finish_prune(&fence)
        .await
        .map_err(|error| ConformanceError::operation("finishing a logical prune", error))?;

    pins.finish_collection(&collector)
        .await
        .map_err(|error| ConformanceError::operation("finishing collection", error))?;
    let finished = inventory().await?;
    require(
        finished.collector.is_none() && finished.logical_prune.is_none(),
        "a finished collection left its ownership behind",
    )?;
    require(
        !finished.pins.contains_key(&pin) && finished.pins.contains_key(&beside),
        "finishing collection did not discard exactly the released history",
    )?;
    let after = pins
        .claim_deletions(finished.revision, BTreeSet::from([held]))
        .await
        .map_err(|error| ConformanceError::operation("claiming retired data", error))?
        .ok_or_else(|| {
            ConformanceError::violation("released history still protected data after collection")
        })?;
    pins.finish_deletions(&after)
        .await
        .map_err(|error| ConformanceError::operation("finishing the last claim", error))?;
    pins.release(&beside)
        .await
        .map_err(|error| ConformanceError::operation("releasing the last pin", error))
}

async fn verified_blob(bytes: &[u8]) -> Result<VerifiedObject, ConformanceError> {
    let key = ObjectKey::blob(BlobId::new(Digest::hash(bytes)));
    let mut reader = Cursor::new(bytes.to_vec());
    FormatRegistry::builtin()
        .verify(
            &key,
            &mut reader as &mut dyn PayloadReader,
            &FormatLimits::default(),
        )
        .await
        .map_err(|error| ConformanceError::operation("building a verified fixture", error))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use object_store::memory::InMemory;
    use object_store::path::Path;

    use super::*;
    use crate::{ChunkedBlobStore, CombinedBlobStore, MemoryBlobStore, MemoryMetadataStore};

    fn chunked(avg: u32) -> ChunkedBlobStore {
        ChunkedBlobStore::new(Arc::new(InMemory::new()), Path::default(), avg)
    }

    #[tokio::test]
    async fn all_blob_store_implementations_share_the_contract() {
        check_blob_store(&MemoryBlobStore::new()).await.unwrap();
        check_blob_store(&chunked(64 * 1024)).await.unwrap();
        check_blob_store(&CombinedBlobStore::new(
            MemoryBlobStore::new(),
            MemoryBlobStore::new(),
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn chunked_store_shares_the_sync_contract() {
        check_blob_sync(&chunked(16 * 1024), &chunked(16 * 1024))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn memory_state_store_shares_the_contract() {
        check_state_store(&MemoryMetadataStore::new().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sqlite_state_store_shares_the_contract() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::TursoMetadataStore::open(directory.path().join("conformance.sqlite"))
            .await
            .unwrap();
        check_state_store(&store).await.unwrap();
    }

    #[tokio::test]
    async fn memory_pin_store_shares_the_contract() {
        check_pin_store(&crate::metadata::MemoryPinStore::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn file_pin_store_shares_the_contract() {
        let directory = tempfile::tempdir().unwrap();
        let pins = crate::metadata::FilePinStore::new(directory.path().join("pins"));
        check_pin_store(&pins).await.unwrap();
    }

    #[tokio::test]
    async fn object_pin_store_shares_the_contract() {
        let pins = crate::metadata::ObjectPinStore::new(
            std::sync::Arc::new(object_store::memory::InMemory::new()),
            object_store::path::Path::from("pins"),
        );
        check_pin_store(&pins).await.unwrap();
    }

    #[cfg(feature = "s3")]
    #[tokio::test]
    async fn chroma_pin_store_shares_the_contract() {
        let directory = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(chroma_storage::Storage::Local(
            chroma_storage::local::LocalStorage::new(directory.path().to_str().unwrap()),
        ));
        let pins = crate::metadata::chroma_pin_store(storage, "pins".into());
        check_pin_store(pins.as_ref()).await.unwrap();
    }

    #[cfg(feature = "s3")]
    #[tokio::test]
    async fn wal3_state_store_shares_the_contract() {
        let directory = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(chroma_storage::Storage::Local(
            chroma_storage::local::LocalStorage::new(directory.path().to_str().unwrap()),
        ));
        let store =
            crate::metadata::Wal3MetadataStore::open(storage, "conformance/state", "writer")
                .await
                .unwrap();
        check_state_store(&store).await.unwrap();
    }
}
