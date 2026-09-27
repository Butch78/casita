#![cfg(all(feature = "native", feature = "experimental"))]

use async_trait::async_trait;
use casita::experimental::{
    BlobStore as _, CommitResult, MetadataError, MetadataMutation, MetadataSnapshot, MetadataStore,
    Repository, RepositoryRevision, TursoMetadataStore, flush_repository_leases,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

struct PausingCommit {
    inner: TursoMetadataStore,
    first: AtomicBool,
    entered: Arc<Notify>,
    resume: Arc<Notify>,
    after_commit: bool,
    fail: bool,
}

#[async_trait]
impl MetadataStore for PausingCommit {
    async fn try_collection_lease(
        &self,
    ) -> Result<Option<casita::experimental::RepositoryLease>, MetadataError> {
        self.inner.try_collection_lease().await
    }
    async fn pin_store(
        &self,
    ) -> Result<
        std::sync::Arc<dyn casita::experimental::PinStore>,
        casita::experimental::MetadataError,
    > {
        self.inner.pin_store().await
    }

    fn coordinates_payload_catalog(&self) -> bool {
        true
    }
    async fn snapshot(&self) -> Result<Arc<dyn MetadataSnapshot>, MetadataError> {
        self.inner.snapshot().await
    }
    async fn commit(
        &self,
        revision: &RepositoryRevision,
        mutation: MetadataMutation,
    ) -> Result<CommitResult, MetadataError> {
        if self.first.swap(false, Ordering::SeqCst) {
            if self.after_commit {
                let result = self.inner.commit(revision, mutation).await;
                self.entered.notify_one();
                self.resume.notified().await;
                return result;
            }
            self.entered.notify_one();
            self.resume.notified().await;
            if self.fail {
                return Err(MetadataError::Backend("injected failure".into()));
            }
        }
        self.inner.commit(revision, mutation).await
    }
}

#[tokio::test]
async fn cancelled_publication_keeps_its_pin_through_online_collection_and_shutdown() {
    for (after_commit, fail) in [(false, false), (true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let base = Repository::local(root.path()).await.unwrap();
        let collector = Repository::local(root.path()).await.unwrap();
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let repository = Repository::new(
            base.payloads().clone(),
            PausingCommit {
                inner: base.metadata().clone(),
                first: AtomicBool::new(true),
                entered: entered.clone(),
                resume: resume.clone(),
                after_commit,
                fail,
            },
        )
        .with_fs_coordination(root.path());
        let mutation = repository.mutation_session().await.unwrap();
        let object = mutation.stage_blob(b"cancelled publication").await.unwrap();
        let key = object.record().key().clone();
        let payload = object.record().payload();
        {
            let publish = mutation.publish_rooted(
                vec![object],
                casita::RootName::try_from("cancelled").unwrap(),
                key.clone(),
            );
            tokio::pin!(publish);
            tokio::select! {
                _ = entered.notified() => {},
                result = &mut publish => panic!("publication unexpectedly finished: {result:?}"),
            }
        }
        drop(mutation);
        drop(repository);
        tokio::time::timeout(std::time::Duration::from_secs(5), collector.try_collect())
            .await
            .expect("collection must proceed during cancelled publication")
            .unwrap();
        assert!(
            !base
                .metadata()
                .pin_store()
                .await
                .unwrap()
                .inventory()
                .await
                .unwrap()
                .pins
                .is_empty()
        );
        assert_eq!(
            base.payloads().read_to_vec(&payload).await.unwrap(),
            Some(b"cancelled publication".to_vec())
        );
        let drain = flush_repository_leases();
        tokio::pin!(drain);
        assert!(futures::poll!(&mut drain).is_pending());
        resume.notify_one();
        drain.await.unwrap();
        // Retry through the same packed backend, where a stranded prepared
        // catalog would reject every later publication.
        let retry = base.mutation_session().await.unwrap();
        let object = retry.stage_blob(b"same backend retry").await.unwrap();
        retry.publish_unrooted(vec![object]).await.unwrap();
        drop(retry);
        drop(base);
        collector.collect().await.unwrap();
        let reopened = Repository::local(root.path()).await.unwrap();
        if !fail {
            let hold = reopened.retention_hold().await.unwrap();
            assert!(hold.open_payload(&key).await.unwrap().is_some());
        }
        let mutation = reopened.mutation_session().await.unwrap();
        let object = mutation
            .stage_blob(b"retry after cancellation")
            .await
            .unwrap();
        mutation.publish_unrooted(vec![object]).await.unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_logical_prune_keeps_admission_fenced_until_commit_settles() {
    use casita::experimental::{DataPin, PinScope};
    use std::collections::BTreeSet;

    for (after_commit, fail) in [(false, false), (true, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let base = Repository::local(root.path()).await.unwrap();
        let mutation = base.mutation_session().await.unwrap();
        let object = mutation.stage_blob(b"prune candidate").await.unwrap();
        let key = object.record().key().clone();
        mutation.publish_unrooted(vec![object]).await.unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
        let entered = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let repository = Repository::new(
            base.payloads().clone(),
            PausingCommit {
                inner: base.metadata().clone(),
                first: AtomicBool::new(true),
                entered: entered.clone(),
                resume: resume.clone(),
                after_commit,
                fail,
            },
        )
        .with_fs_coordination(root.path());
        let ledger = base.metadata().pin_store().await.unwrap();
        {
            let collect = repository.collect_logical();
            tokio::pin!(collect);
            tokio::select! {
                _ = entered.notified() => {},
                result = &mut collect => panic!("prune unexpectedly finished: {result:?}"),
            }
        }
        drop(repository);
        let candidate = DataPin {
            scope: PinScope::Closures(BTreeSet::from([key.clone()])),
            catalog: None,
            resources: BTreeSet::new(),
        };
        assert!(ledger.inventory().await.unwrap().logical_prune.is_some());
        assert!(ledger.register(candidate.clone()).await.unwrap().is_none());
        let drain = flush_repository_leases();
        tokio::pin!(drain);
        assert!(futures::poll!(&mut drain).is_pending());
        resume.notify_one();
        drain.await.unwrap();
        assert!(ledger.inventory().await.unwrap().logical_prune.is_none());
        let snapshot = base.metadata().snapshot().await.unwrap();
        assert_eq!(snapshot.object(&key).await.unwrap().is_some(), fail);
        let token = ledger.register(candidate).await.unwrap().unwrap();
        ledger.release(&token).await.unwrap();
    }
}
