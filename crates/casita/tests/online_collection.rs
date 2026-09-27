#![cfg(all(feature = "native", feature = "experimental"))]

use async_trait::async_trait;
use casita::experimental::{
    BlobBatchGuard, BlobGc, BlobId, BlobReader, BlobStore, BlobWriter, ChunkId, DataPin,
    DataPinLease, Error, MemoryBlobStore, MemoryMetadataStore, MetadataStore, PinResource,
    PinScope, Repository, flush_repository_leases,
};
use futures::stream::BoxStream;
use std::{collections::BTreeSet, sync::Arc};
use tokio::sync::Notify;

#[derive(Clone)]
struct PausedGc {
    inner: MemoryBlobStore,
    deleting: Arc<Notify>,
    resume_delete: Arc<Notify>,
    finishing: Arc<Notify>,
    resume_finish: Arc<Notify>,
    audit: Option<(Arc<Notify>, Arc<Notify>)>,
}

#[async_trait]
impl BlobStore for PausedGc {
    fn publication(&self) -> casita::experimental::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> casita::experimental::BackendWriteScope {
        self.inner.write_scope()
    }

    fn begin_pinned_batch(&self, pin: DataPinLease) -> Result<BlobBatchGuard, Error> {
        self.inner.begin_pinned_batch(pin)
    }
    async fn has(&self, blob: &BlobId) -> Result<bool, Error> {
        self.inner.has(blob).await
    }
    async fn open_read(&self, blob: &BlobId) -> Result<Option<Box<dyn BlobReader>>, Error> {
        self.inner.open_read(blob).await
    }
    async fn open_write(&self) -> Box<dyn BlobWriter> {
        self.inner.open_write().await
    }
}

#[async_trait]
impl BlobGc for PausedGc {
    async fn open_read_for_fsck(
        &self,
        blob: &BlobId,
        present: bool,
    ) -> Result<Option<Box<dyn BlobReader>>, Error> {
        if let Some((entered, resume)) = &self.audit {
            entered.notify_one();
            resume.notified().await;
        }
        self.inner.open_read_for_fsck(blob, present).await
    }

    async fn reclaim_metadata_pinned(
        &self,
        pins: Arc<dyn casita::experimental::PinStore>,
        owned_claims: std::collections::BTreeSet<casita::experimental::PinToken>,
    ) -> Result<(), Error> {
        self.inner.reclaim_metadata_pinned(pins, owned_claims).await
    }

    fn list_blobs(&self) -> BoxStream<'_, Result<BlobId, Error>> {
        self.inner.list_blobs()
    }
    fn list_chunks(&self) -> BoxStream<'_, Result<ChunkId, Error>> {
        self.inner.list_chunks()
    }
    async fn delete_blob(&self, blob: &BlobId) -> Result<(), Error> {
        self.deleting.notify_one();
        self.resume_delete.notified().await;
        self.inner.delete_blob(blob).await
    }
    async fn delete_chunk(&self, chunk: &ChunkId) -> Result<(), Error> {
        self.inner.delete_chunk(chunk).await
    }
    async fn finish_collection(&self, _: bool) -> Result<(), Error> {
        self.finishing.notify_one();
        self.resume_finish.notified().await;
        Ok(())
    }
    async fn delete_blobs_pinned(
        &self,
        blobs: &[BlobId],
        pins: Arc<dyn casita::experimental::PinStore>,
        owned_claims: BTreeSet<casita::experimental::PinToken>,
        before_prune: bool,
    ) -> Result<usize, Error> {
        self.deleting.notify_one();
        self.resume_delete.notified().await;
        self.inner
            .delete_blobs_pinned(blobs, pins, owned_claims, before_prune)
            .await
    }
    async fn delete_chunks_pinned(
        &self,
        chunks: &[ChunkId],
        pins: Arc<dyn casita::experimental::PinStore>,
        owned_claims: BTreeSet<casita::experimental::PinToken>,
    ) -> Result<usize, Error> {
        self.inner
            .delete_chunks_pinned(chunks, pins, owned_claims)
            .await
    }
    async fn finish_deletions_pinned(
        &self,
        force_reclaim: bool,
        pins: Arc<dyn casita::experimental::PinStore>,
        owned_claims: BTreeSet<casita::experimental::PinToken>,
        before_prune: bool,
    ) -> Result<(), Error> {
        self.inner
            .finish_deletions_pinned(force_reclaim, pins, owned_claims, before_prune)
            .await
    }
    async fn finish_collection_pinned(
        &self,
        force_reclaim: bool,
        _pins: Arc<dyn casita::experimental::PinStore>,
        _owned_claims: BTreeSet<casita::experimental::PinToken>,
    ) -> Result<(), Error> {
        self.finish_collection(force_reclaim).await
    }
}

fn staging(blob: BlobId) -> DataPin {
    DataPin {
        scope: PinScope::Staging,
        catalog: None,
        resources: BTreeSet::from([PinResource::Blob(blob)]),
    }
}

#[tokio::test]
async fn deletion_claims_cover_cancelled_io_and_deferred_collection_finish() {
    for cancel in [false, true] {
        let payloads = PausedGc {
            inner: MemoryBlobStore::new(),
            deleting: Arc::default(),
            resume_delete: Arc::default(),
            finishing: Arc::default(),
            resume_finish: Arc::default(),
            audit: None,
        };
        let repository = Arc::new(Repository::new(
            payloads.clone(),
            MemoryMetadataStore::new().unwrap(),
        ));
        let mutation = repository.mutation_session().await.unwrap();
        let object = mutation.stage_blob(b"deletion candidate").await.unwrap();
        let blob = object.record().payload();
        mutation.publish_unrooted(vec![object]).await.unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
        let ledger = repository.metadata().pin_store().await.unwrap();
        let collector = repository.clone();
        let task = tokio::spawn(async move { collector.collect().await });
        payloads.deleting.notified().await;
        let claimed = ledger.inventory().await.unwrap();
        assert_eq!(claimed.deletions.len(), 1);
        assert!(
            claimed
                .deletions
                .values()
                .next()
                .unwrap()
                .contains(&PinResource::Blob(blob))
        );
        assert!(ledger.register(staging(blob)).await.unwrap().is_none());
        // The public facade must admit a post-prune reader and an unrelated
        // writer while this collector still owns its in-process mutex.
        let (read_hold, survivor) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let hold = repository.retention_hold().await.unwrap();
                let mutation = repository.mutation_session().await.unwrap();
                let staged = mutation
                    .stage_blob(b"published during physical deletion")
                    .await
                    .unwrap();
                let key = staged.record().key().clone();
                mutation
                    .publish_rooted(
                        vec![staged],
                        "online/survivor".parse().unwrap(),
                        key.clone(),
                    )
                    .await
                    .unwrap();
                (hold, key)
            })
            .await
            .expect("scoped holds must not wait for a physical sweep");
        assert!(
            read_hold
                .snapshot()
                .object(&survivor)
                .await
                .unwrap()
                .is_none()
        );
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert!(payloads.inner.has(&blob).await.unwrap());
            payloads.resume_delete.notify_one();
            payloads.finishing.notified().await;
            assert!(!payloads.inner.has(&blob).await.unwrap());
            assert_eq!(
                ledger.inventory().await.unwrap().deletions,
                claimed.deletions
            );
            let drain = flush_repository_leases();
            tokio::pin!(drain);
            assert!(futures::poll!(&mut drain).is_pending());
            payloads.resume_finish.notify_one();
            drain.await.unwrap();
            assert!(ledger.inventory().await.unwrap().deletions.is_empty());
        } else {
            payloads.resume_delete.notify_one();
            payloads.finishing.notified().await;
            assert!(!payloads.inner.has(&blob).await.unwrap());
            assert_eq!(
                ledger.inventory().await.unwrap().deletions,
                claimed.deletions
            );
            assert!(ledger.register(staging(blob)).await.unwrap().is_none());
            let unrelated = ledger
                .register(staging(BlobId::new(casita::Digest::hash(b"unrelated"))))
                .await
                .unwrap()
                .unwrap();
            payloads.resume_finish.notify_one();
            task.await.unwrap().unwrap();
            assert!(ledger.inventory().await.unwrap().deletions.is_empty());
            assert!(
                ledger
                    .inventory()
                    .await
                    .unwrap()
                    .pins
                    .contains_key(&unrelated)
            );
            ledger.release(&unrelated).await.unwrap();
        }
        assert!(matches!(
            repository.verify_closure(&survivor).await.unwrap(),
            casita::experimental::ClosureStatus::Complete { objects: 1 }
        ));
        drop(read_hold);
        flush_repository_leases().await.unwrap();
    }
}

#[tokio::test]
async fn integrity_scan_pins_its_snapshot_without_blocking_unrelated_collection() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let payloads = PausedGc {
            inner: MemoryBlobStore::new(),
            deleting: Arc::default(),
            resume_delete: Arc::default(),
            finishing: Arc::default(),
            resume_finish: Arc::default(),
            audit: Some((entered.clone(), resume.clone())),
        };
        let state = MemoryMetadataStore::new().unwrap();
        let repository = Arc::new(Repository::new(payloads.clone(), state.clone()));
        let mutation = repository.mutation_session().await.unwrap();
        let old = mutation.stage_blob(b"audit witness").await.unwrap();
        let old_blob = old.record().payload();
        let key = old.record().key().clone();
        let root = casita::RootName::try_from("audit").unwrap();
        mutation
            .publish_rooted(vec![old], root.clone(), key)
            .await
            .unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
        let revision = repository.metadata().snapshot().await.unwrap().revision();
        let auditor = repository.clone();
        let audit = tokio::spawn(async move { auditor.fsck().await });
        entered.notified().await;

        let mutation = repository.mutation_session().await.unwrap();
        let later = mutation.stage_blob(b"later garbage").await.unwrap();
        let later_blob = later.record().payload();
        mutation
            .publish(
                vec![later],
                vec![casita::experimental::RootChange::Remove { name: root }],
            )
            .await
            .unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
        let collector = repository.clone();
        let collection = tokio::spawn(async move { collector.collect().await });
        payloads.deleting.notified().await;
        payloads.resume_delete.notify_one();
        payloads.finishing.notified().await;
        payloads.resume_finish.notify_one();
        let outcome = collection.await.unwrap().unwrap();
        assert_eq!(outcome.removed.logical_objects, 1);
        assert!(!payloads.inner.has(&later_blob).await.unwrap());
        assert!(payloads.inner.has(&old_blob).await.unwrap());

        resume.notify_one();
        let report = audit.await.unwrap().unwrap();
        assert_eq!(report.revision, revision);
        assert!(report.is_clean(), "{report:?}");
        flush_repository_leases().await.unwrap();
        let unpaused = Repository::new(payloads.inner, state);
        assert_eq!(unpaused.collect().await.unwrap().removed.logical_objects, 1);
        assert!(!unpaused.payloads().has(&old_blob).await.unwrap());
    })
    .await
    .expect("online scan must allow unrelated collection to finish");
}
