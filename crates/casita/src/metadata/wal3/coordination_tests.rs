// Included in wal3::tests to share the isolated RustFS fixtures.

#[tokio::test]
async fn remote_cancelled_commit_keeps_its_hold_until_publication_finishes() {
    use crate::flush_repository_leases;
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let mut writer = Wal3MetadataStore::open(storage.clone(), "cancel-commit/state", "writer")
        .await
        .unwrap();
    let collector = Wal3MetadataStore::open(storage, "cancel-commit/state", "collector")
        .await
        .unwrap();
    let hold = writer.try_collection_lease().await.unwrap().unwrap();
    let mut expected = writer.snapshot().await.unwrap().revision();
    for _ in 0..MAX_TAIL_DELTAS {
        expected = writer
            .commit(&expected, MetadataMutation::new())
            .await
            .unwrap()
            .revision;
    }
    let object = verified_blob(b"publication survives cancelled caller").await;
    let key = object.record().key().clone();
    let pause = Arc::new(CheckpointPause::default());
    writer.checkpoint_pause = Some(pause.clone());
    let commit = tokio::spawn(async move {
        let mut mutation = MetadataMutation::new();
        mutation.add_object(object);
        writer.commit(&expected, mutation).await
    });
    tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    commit.abort();
    assert!(commit.await.unwrap_err().is_cancelled());
    drop(hold);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), flush_repository_leases())
            .await
            .is_err()
    );
    assert!(collector.try_collection_lease().await.unwrap().is_none());
    pause.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(10), flush_repository_leases())
        .await
        .unwrap()
        .unwrap();
    let exclusive = collector.try_collection_lease().await.unwrap().unwrap();
    assert!(
        collector
            .snapshot()
            .await
            .unwrap()
            .object(&key)
            .await
            .unwrap()
            .is_some()
    );
    drop(exclusive);
    flush_repository_leases().await.unwrap();
}

#[derive(Clone)]
struct PausedRemoteDeletion {
    inner: crate::MemoryBlobStore,
    pause: Arc<CheckpointPause>,
    pause_once: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl crate::BlobStore for PausedRemoteDeletion {
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }

    fn begin_pinned_batch(
        &self,
        pin: crate::metadata::DataPinLease,
    ) -> Result<crate::blob::BlobBatchGuard, crate::error::Error> {
        self.inner.begin_pinned_batch(pin)
    }

    async fn has(&self, id: &crate::BlobId) -> Result<bool, crate::error::Error> {
        self.inner.has(id).await
    }
    async fn open_read(
        &self,
        id: &crate::BlobId,
    ) -> Result<Option<Box<dyn crate::BlobReader>>, crate::error::Error> {
        self.inner.open_read(id).await
    }
    async fn open_write(&self) -> Box<dyn crate::BlobWriter> {
        self.inner.open_write().await
    }
}

#[async_trait]
impl crate::BlobGc for PausedRemoteDeletion {
    async fn reclaim_metadata_pinned(
        &self,
        pins: Arc<dyn crate::metadata::PinStore>,
        owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<(), crate::error::Error> {
        self.inner.reclaim_metadata_pinned(pins, owned_claims).await
    }

    fn list_blobs(&self) -> BoxStream<'_, Result<crate::BlobId, crate::error::Error>> {
        self.inner.list_blobs()
    }
    fn list_chunks(&self) -> BoxStream<'_, Result<crate::ChunkId, crate::error::Error>> {
        self.inner.list_chunks()
    }
    async fn delete_blob(&self, id: &crate::BlobId) -> Result<(), crate::error::Error> {
        if self.pause_once.swap(false, Ordering::SeqCst) {
            self.pause.reached.notify_one();
            self.pause.resume.notified().await;
        }
        self.inner.delete_blob(id).await
    }
    async fn delete_chunk(&self, id: &crate::ChunkId) -> Result<(), crate::error::Error> {
        self.inner.delete_chunk(id).await
    }
    async fn delete_blobs_pinned(
        &self,
        ids: &[crate::BlobId],
        pins: Arc<dyn crate::metadata::PinStore>,
        owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
        before_prune: bool,
    ) -> Result<usize, crate::error::Error> {
        if self.pause_once.swap(false, Ordering::SeqCst) {
            self.pause.reached.notify_one();
            self.pause.resume.notified().await;
        }
        self.inner
            .delete_blobs_pinned(ids, pins, owned_claims, before_prune)
            .await
    }
    async fn delete_chunks_pinned(
        &self,
        ids: &[crate::ChunkId],
        pins: Arc<dyn crate::metadata::PinStore>,
        owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<usize, crate::error::Error> {
        self.inner.delete_chunks_pinned(ids, pins, owned_claims).await
    }
    async fn finish_deletions_pinned(
        &self,
        force_reclaim: bool,
        pins: Arc<dyn crate::metadata::PinStore>,
        owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
        before_prune: bool,
    ) -> Result<(), crate::error::Error> {
        self.inner
            .finish_deletions_pinned(force_reclaim, pins, owned_claims, before_prune)
            .await
    }
    async fn finish_collection_pinned(
        &self,
        force_reclaim: bool,
        pins: Arc<dyn crate::metadata::PinStore>,
        owned_claims: std::collections::BTreeSet<crate::metadata::PinToken>,
    ) -> Result<(), crate::error::Error> {
        self.inner
            .finish_collection_pinned(force_reclaim, pins, owned_claims)
            .await
    }
}

#[tokio::test]
async fn remote_cancelled_physical_collection_keeps_claims_until_io_settles() {
    use crate::{BlobStore as _, flush_repository_leases, repository::Repository};
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let state = Wal3MetadataStore::open(storage, "cancel-collection/state", "collector")
        .await
        .unwrap();
    let pause = Arc::new(CheckpointPause::default());
    let payloads = PausedRemoteDeletion {
        inner: crate::MemoryBlobStore::new(),
        pause: pause.clone(),
        pause_once: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    let repository = Repository::new(payloads.clone(), state.clone());
    let dead = payloads.put_slice(b"unreachable payload").await.unwrap();
    let collection = tokio::spawn(async move { repository.collect().await });
    tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    collection.abort();
    assert!(collection.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), flush_repository_leases())
            .await
            .is_err()
    );
    let holds = state.repository_holds().await.unwrap();
    assert_eq!(holds.len(), 1);
    assert!(holds[0].exclusive);
    assert!(state.try_collection_lease().await.unwrap().is_none());
    assert!(payloads.has(&dead).await.unwrap());
    let ledger = state.pin_store().await.unwrap();
    let inventory = ledger.inventory().await.unwrap();
    assert!(inventory.collector.is_some());
    assert!(
        inventory
            .deletions
            .values()
            .any(|resources| { resources.contains(&crate::metadata::PinResource::Blob(dead)) })
    );
    // Cancellation ended only the caller. The tracked collector still owns
    // the paused I/O, its exact deletion claim, and its collector history.
    pause.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(10), flush_repository_leases())
        .await
        .unwrap()
        .unwrap();
    assert!(!payloads.has(&dead).await.unwrap());
    let inventory = ledger.inventory().await.unwrap();
    assert!(inventory.collector.is_none());
    assert!(inventory.deletions.is_empty());
    state
        .collect_repository_coordination(Duration::ZERO)
        .await
        .unwrap();
    flush_repository_leases().await.unwrap();
    assert!(state.repository_holds().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_cancelled_admission_releases_only_after_its_cas_completes() {
    use crate::flush_repository_leases;
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let first = Wal3MetadataStore::open(storage.clone(), "cancel/state", "first")
        .await
        .unwrap();
    let second = Wal3MetadataStore::open(storage, "cancel/state", "second")
        .await
        .unwrap();
    let pause = Arc::new(CheckpointPause::default());
    first
        .repository_coordination()
        .await
        .unwrap()
        .pause_after_admission(pause.clone());
    let task = tokio::spawn(async move { first.try_collection_lease().await });
    tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert!(second.try_collection_lease().await.unwrap().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), flush_repository_leases())
            .await
            .is_err()
    );
    pause.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(10), flush_repository_leases())
        .await
        .unwrap()
        .unwrap();
    assert!(second.repository_holds().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_gc_preserves_online_staging_and_refreshes_its_catalog() {
    use crate::{BlobStore as _, flush_repository_leases};
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let endpoint = rustfs.endpoint();
    let writer = rustfs_repository(
        storage.clone(),
        "casita-test",
        &endpoint,
        "admission",
        "writer",
    )
    .await;
    let collector =
        rustfs_repository(storage, "casita-test", &endpoint, "admission", "collector").await;
    let session = writer.mutation_session().await.unwrap();
    let staged = session
        .stage_blob(b"uploaded before publication")
        .await
        .unwrap();
    let key = staged.record().key().clone();
    let payload = staged.record().payload();
    writer.payloads().flush().await.unwrap();
    assert_eq!(
        collector
            .try_collect()
            .await
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    flush_repository_leases().await.unwrap();
    assert_eq!(
        collector
            .try_vacuum()
            .await
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    let root = RootName::try_from("retained/current").unwrap();
    session
        .publish_rooted(vec![staged], root, key.clone())
        .await
        .unwrap();
    drop(session);
    flush_repository_leases().await.unwrap();
    // The collector was opened before the writer uploaded its pack. Its old
    // catalog must be refreshed after exclusive admission, before marking.
    collector.collect().await.unwrap();
    flush_repository_leases().await.unwrap();
    assert_eq!(
        collector
            .payloads()
            .read_to_vec(&payload)
            .await
            .unwrap()
            .unwrap(),
        b"uploaded before publication"
    );
    assert!(matches!(
        collector.verify_closure(&key).await.unwrap(),
        crate::ClosureStatus::Complete { .. }
    ));
    flush_repository_leases().await.unwrap();
}

#[tokio::test]
async fn remote_gc_preserves_an_old_retained_read_after_root_removal() {
    use crate::{BlobStore as _, flush_repository_leases};
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let endpoint = rustfs.endpoint();
    let reader = rustfs_repository(
        storage.clone(),
        "casita-test",
        &endpoint,
        "reader",
        "reader",
    )
    .await;
    let collector =
        rustfs_repository(storage, "casita-test", &endpoint, "reader", "collector").await;
    let root = RootName::try_from("retained/current").unwrap();
    let session = reader.mutation_session().await.unwrap();
    let staged = session.stage_blob(b"old snapshot bytes").await.unwrap();
    let key = staged.record().key().clone();
    let payload = staged.record().payload();
    session
        .publish_rooted(vec![staged], root.clone(), key.clone())
        .await
        .unwrap();
    drop(session);
    flush_repository_leases().await.unwrap();
    let hold = reader.owned_retention_hold().await.unwrap();
    collector.remove_root_if_matches(&root, &key).await.unwrap();
    flush_repository_leases().await.unwrap();
    assert_eq!(
        collector
            .try_collect()
            .await
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    flush_repository_leases().await.unwrap();
    assert_eq!(
        collector
            .try_collect_logical()
            .await
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    assert!(hold.object(&key).await.unwrap().is_some());
    assert_eq!(
        reader
            .payloads()
            .read_to_vec(&payload)
            .await
            .unwrap()
            .unwrap(),
        b"old snapshot bytes"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), collector.collect())
            .await
            .unwrap()
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    drop(hold);
    flush_repository_leases().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), collector.collect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.removed.logical_objects, 1);
    flush_repository_leases().await.unwrap();
    assert!(!collector.payloads().has(&payload).await.unwrap());
}

#[tokio::test]
async fn remote_admission_serializes_collectors_but_allows_online_pins() {
    use crate::flush_repository_leases;
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let first = Wal3MetadataStore::open(storage.clone(), "race/state", "same-writer")
        .await
        .unwrap();
    let second = Wal3MetadataStore::open(storage, "race/state", "same-writer")
        .await
        .unwrap();
    // Diagnostic writer names never grant or conflate ownership.
    for _ in 0..2 {
        let (left, right) =
            tokio::join!(first.try_collection_lease(), second.try_collection_lease());
        let left = left.unwrap();
        let right = right.unwrap();
        assert_ne!(left.is_some(), right.is_some());
        assert_eq!(first.repository_holds().await.unwrap().len(), 1);
        drop((left, right));
        flush_repository_leases().await.unwrap();
        assert!(first.repository_holds().await.unwrap().is_empty());
    }
    let pin = |store: Wal3MetadataStore| async move {
        crate::metadata::DataPinLease::acquire(
            store.pin_store().await.unwrap(),
            crate::metadata::DataPin {
                scope: crate::metadata::PinScope::Staging,
                catalog: None,
                resources: BTreeSet::new(),
            },
        )
        .await
        .unwrap()
    };
    let first_pin = pin(first.clone()).await;
    let second_pin = pin(second.clone()).await;
    let collector = first.try_collection_lease().await.unwrap().unwrap();
    assert_eq!(
        first
            .pin_store()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
            .pins
            .len(),
        2
    );
    drop((collector, first_pin, second_pin));
    flush_repository_leases().await.unwrap();
}

#[tokio::test]
async fn remote_abandoned_holds_survive_reopen_and_recovery_is_token_specific() {
    use crate::flush_repository_leases;
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let first = Wal3MetadataStore::open(storage.clone(), "abandoned/state", "worker")
        .await
        .unwrap();
    let mut hold = first.try_collection_lease().await.unwrap().unwrap();
    let old = first.repository_holds().await.unwrap().pop().unwrap();
    hold.retain_on_drop();
    drop(hold); // models interrupted collection: no durable release
    drop(first);
    let restarted = Wal3MetadataStore::open(storage, "abandoned/state", "worker")
        .await
        .unwrap();
    assert!(restarted.try_collection_lease().await.unwrap().is_none());
    assert!(restarted.try_collection_lease().await.unwrap().is_none());
    assert!(
        restarted
            .release_abandoned_repository_hold(&old.token)
            .await
            .unwrap()
    );
    let current = restarted.try_collection_lease().await.unwrap().unwrap();
    let new = restarted.repository_holds().await.unwrap().pop().unwrap();
    assert_ne!(new.token, old.token);
    assert!(
        !restarted
            .release_abandoned_repository_hold(&old.token)
            .await
            .unwrap()
    );
    assert_eq!(restarted.repository_holds().await.unwrap(), vec![new]);
    drop(current);
    flush_repository_leases().await.unwrap();
}

#[tokio::test]
async fn remote_state_log_gc_runs_with_repository_readers() {
    use crate::flush_repository_leases;
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let reader = Wal3MetadataStore::open(storage.clone(), "state-gc/state", "reader")
        .await
        .unwrap();
    let collector = Wal3MetadataStore::open(storage, "state-gc/state", "collector")
        .await
        .unwrap();
    let repository = crate::repository::Repository::new(crate::MemoryBlobStore::new(), reader);
    let hold = repository.retention_hold().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        collector.collect_wal(Duration::ZERO),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        hold.snapshot()
            .roots()
            .try_collect::<Vec<_>>()
            .await
            .unwrap()
            .is_empty()
    );
    drop(hold);
    flush_repository_leases().await.unwrap();
}

struct RejectedCollectionCommit {
    inner: Wal3MetadataStore,
    reject: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl MetadataStore for RejectedCollectionCommit {
    async fn pin_store(&self) -> Result<Arc<dyn crate::metadata::PinStore>, MetadataError> {
        self.inner.pin_store().await
    }
    async fn try_collection_lease(
        &self,
    ) -> Result<Option<crate::metadata::RepositoryLease>, MetadataError> {
        self.inner.try_collection_lease().await
    }
    fn coordinates_payload_catalog(&self) -> bool {
        true
    }
    async fn snapshot(&self) -> Result<Arc<dyn MetadataSnapshot>, MetadataError> {
        self.inner.snapshot().await
    }
    async fn commit(
        &self,
        expected: &RepositoryRevision,
        mutation: MetadataMutation,
    ) -> Result<CommitResult, MetadataError> {
        if mutation.retained_objects.is_some() && self.reject.load(Ordering::SeqCst) {
            return Err(MetadataError::Backend(
                "injected logical prune rejection".into(),
            ));
        }
        self.inner.commit(expected, mutation).await
    }
}

#[tokio::test]
async fn remote_rejected_logical_prune_releases_collection_hold() {
    use crate::{flush_repository_leases, repository::Repository};
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let state = Wal3MetadataStore::open(storage, "reject-collection/state", "collector")
        .await
        .unwrap();
    let reject = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let repository = Repository::new(
        crate::MemoryBlobStore::new(),
        RejectedCollectionCommit {
            inner: state.clone(),
            reject: reject.clone(),
        },
    );
    let mutation = repository.mutation_session().await.unwrap();
    let object = mutation
        .stage_blob(b"unrooted record to prune")
        .await
        .unwrap();
    mutation.publish_unrooted(vec![object]).await.unwrap();
    drop(mutation);
    flush_repository_leases().await.unwrap();
    reject.store(true, Ordering::SeqCst);
    let error = repository.collect().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected logical prune rejection"),
        "{error}"
    );
    flush_repository_leases().await.unwrap();
    assert!(state.repository_holds().await.unwrap().is_empty());
    reject.store(false, Ordering::SeqCst);
    repository.collect().await.unwrap();
    flush_repository_leases().await.unwrap();
    assert!(state.repository_holds().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_catalog_maintenance_retains_admission_through_publish_or_discard() {
    use crate::object_store::{ObjectStore, memory::InMemory, path::Path};
    use crate::{ChunkedBlobStore, RootName, flush_repository_leases, repository::Repository};
    use tokio::io::AsyncReadExt;

    async fn payloads(
        objects: Arc<dyn ObjectStore>,
        state: &Wal3MetadataStore,
    ) -> ChunkedBlobStore {
        let catalog = state
            .snapshot()
            .await
            .unwrap()
            .payload_catalog()
            .unwrap()
            .to_vec();
        ChunkedBlobStore::packed_with_catalog(objects, Path::from("payloads"), 1024, crate::PackOptions { target_size: u64::MAX, cache_capacity: 0 }, &catalog)
        .await
        .unwrap()
    }

    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    for competing_write in [false, true] {
        let state = Wal3MetadataStore::open(
            storage.clone(),
            format!("maintenance-{competing_write}/state"),
            "writer",
        )
        .await
        .unwrap();
        let objects: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let writer = Repository::new(payloads(objects.clone(), &state).await, state.clone());
        let pause = writer.pause_catalog_maintenance_for_test();
        let mut expected = Vec::new();
        for name in ["first", "second"] {
            let session = writer.mutation_session().await.unwrap();
            let object = session.stage_blob(name.as_bytes()).await.unwrap();
            let key = object.record().key().clone();
            session
                .publish_rooted(vec![object], RootName::try_from(name).unwrap(), key.clone())
                .await
                .unwrap();
            expected.push((key, name));
            drop(session);
            writer
                .payloads()
                .set_pack_catalog_rebase_run_bytes_for_test(1);
        }
        tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
            .await
            .unwrap();
        drop(writer);
        let collector = Repository::new(payloads(objects.clone(), &state).await, state.clone());
        collector.try_vacuum().await.unwrap();
        if competing_write {
            let session = collector.mutation_session().await.unwrap();
            let object = session.stage_blob(b"competitor").await.unwrap();
            let key = object.record().key().clone();
            session
                .publish_rooted(
                    vec![object],
                    RootName::try_from("competitor").unwrap(),
                    key.clone(),
                )
                .await
                .unwrap();
            expected.push((key, "competitor"));
            drop(session);
        }
        let drain = flush_repository_leases();
        tokio::pin!(drain);
        assert!(futures::poll!(&mut drain).is_pending());
        pause.resume.notify_one();
        tokio::time::timeout(Duration::from_secs(10), drain)
            .await
            .unwrap()
            .unwrap();
        assert!(state.repository_holds().await.unwrap().is_empty());
        collector.vacuum().await.unwrap();
        flush_repository_leases().await.unwrap();
        let reopened = Repository::new(payloads(objects, &state).await, state.clone());
        let hold = reopened.retention_hold().await.unwrap();
        for (key, bytes) in expected {
            let (_, mut reader) = hold.open_payload(&key).await.unwrap().unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).await.unwrap();
            assert_eq!(actual, bytes.as_bytes());
        }
        drop(hold);
        flush_repository_leases().await.unwrap();
    }
}

#[tokio::test]
async fn remote_coordination_gc_preserves_lazy_diagnostic_snapshot() {
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let state = Wal3MetadataStore::open(storage.clone(), "diagnostic-pins/state", "reader")
        .await
        .unwrap();
    let hold = state.try_collection_lease().await.unwrap().unwrap();
    let token = state.repository_holds().await.unwrap()[0].token.clone();
    let coordination = state.repository_coordination().await.unwrap();
    let log = coordination.metadata_store();
    let mut revision = log.snapshot().await.unwrap().revision();
    for _ in 0..MAX_TAIL_DELTAS {
        revision = log
            .commit(&revision, MetadataMutation::new())
            .await
            .unwrap()
            .revision;
    }
    // Open a fresh handle so the diagnostic has not cached the lazy roots.
    let reader = Wal3MetadataStore::open(storage, "diagnostic-pins/state", "diagnostic")
        .await
        .unwrap();
    let (snapshot, pin) = reader
        .repository_coordination()
        .await
        .unwrap()
        .pinned_snapshot()
        .await
        .unwrap();
    let resources = snapshot.retention_resources();
    assert!(resources.len() >= 2);
    drop(hold);
    crate::flush_repository_leases().await.unwrap();
    state
        .collect_repository_coordination(Duration::ZERO)
        .await
        .unwrap();
    crate::flush_repository_leases().await.unwrap();
    assert!(snapshot.root(&token).await.unwrap().is_some());
    let paths = log.object_shards.list_paths().await.unwrap();
    for resource in &resources {
        let crate::metadata::PinResource::MetadataObject(path) = resource else {
            panic!("unexpected resource")
        };
        assert!(paths.contains(path));
    }
    let old_roots = snapshot.roots().try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(old_roots.len(), 1);
    assert_eq!(old_roots[0].name(), &token);
    drop(snapshot);
    drop(pin);
    crate::flush_repository_leases().await.unwrap();
    state
        .collect_repository_coordination(Duration::ZERO)
        .await
        .unwrap();
    crate::flush_repository_leases().await.unwrap();
    let remaining = log.object_shards.list_paths().await.unwrap();
    assert!(
        resources.iter().any(|resource| {
            let crate::metadata::PinResource::MetadataObject(path) = resource else {
                return false;
            };
            !remaining.contains(path)
        }),
        "obsolete diagnostic roots should be reclaimed after the pin drops"
    );
}

#[tokio::test]
async fn remote_coordination_gc_cancellation_releases_ownership_after_io() {
    let _lock = rustfs_test_lock().lock().await;
    let rustfs = Rustfs::start();
    let storage = rustfs_storage(&rustfs).await;
    let state = Wal3MetadataStore::open(storage, "cancel-coordination/state", "collector")
        .await
        .unwrap();
    let coordination = state.repository_coordination().await.unwrap();
    let log = coordination.metadata_store();
    let orphan = encode_object_shard(&[(
        verified_blob(b"orphan coordination shard")
            .await
            .record()
            .clone(),
        false,
        0,
    )])
    .unwrap();
    log.object_shards.put(&orphan).await.unwrap();
    let pause = log.object_shards.pause_deletion();
    let collector = state.clone();
    let task = tokio::spawn(async move {
        collector
            .collect_repository_coordination(Duration::ZERO)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), pause.reached.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(state.try_collection_lease().await.unwrap().is_none());
    assert!(
        !log.pin_store()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
            .deletions
            .is_empty()
    );
    pause.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(10), crate::flush_repository_leases())
        .await
        .unwrap()
        .unwrap();
    assert!(state.repository_holds().await.unwrap().is_empty());
    assert!(
        log.pin_store()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
            .deletions
            .is_empty()
    );
}
