use super::*;
use crate::blob::{BlobBatchGuard, BlobWriter, MemoryBlobStore};
use crate::metadata::{DataPinLease, MemoryMetadataStore};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Clone)]
struct PausedDirectories {
    inner: MemoryBlobStore,
    started: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    window_full: Arc<Notify>,
    resume: Arc<Notify>,
    fail: bool,
}

struct ActiveWrite(Arc<AtomicUsize>);

struct OrderedCheckpoints {
    inner: MemoryMetadataStore,
    limit: usize,
}

#[async_trait]
impl MetadataStore for OrderedCheckpoints {
    async fn try_collection_lease(
        &self,
    ) -> Result<Option<crate::metadata::RepositoryLease>, crate::metadata::MetadataError> {
        self.inner.try_collection_lease().await
    }
    fn coordinates_payload_catalog(&self) -> bool {
        self.inner.coordinates_payload_catalog()
    }
    async fn pin_store(&self) -> Result<Arc<dyn crate::metadata::PinStore>, MetadataError> {
        self.inner.pin_store().await
    }

    async fn snapshot(&self) -> Result<Arc<dyn MetadataSnapshot>, MetadataError> {
        self.inner.snapshot().await
    }

    async fn commit(
        &self,
        revision: &crate::RepositoryRevision,
        mutation: crate::metadata::MetadataMutation,
    ) -> Result<CommitResult, MetadataError> {
        assert!(mutation.objects().len() <= self.limit);
        let snapshot = self.inner.snapshot().await?;
        let mut staged = HashSet::new();
        for object in mutation.objects() {
            for child in object.record().links() {
                assert!(
                    staged.contains(child) || snapshot.object(child).await?.is_some(),
                    "parent published before child: {child:?}"
                );
            }
            staged.insert(object.record().key().clone());
        }
        self.inner.commit(revision, mutation).await
    }
}

impl Drop for ActiveWrite {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl BlobStore for PausedDirectories {
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }

    fn begin_pinned_batch(&self, pin: DataPinLease) -> Result<BlobBatchGuard, crate::error::Error> {
        self.inner.begin_pinned_batch(pin)
    }

    async fn has(&self, digest: &BlobId) -> Result<bool, crate::error::Error> {
        self.inner.has(digest).await
    }

    async fn open_read(
        &self,
        digest: &BlobId,
    ) -> Result<Option<Box<dyn BlobReader>>, crate::error::Error> {
        self.inner.open_read(digest).await
    }

    async fn open_write(&self) -> Box<dyn BlobWriter> {
        self.inner.open_write().await
    }

    async fn put_slice(&self, data: &[u8]) -> Result<BlobId, crate::error::Error> {
        let index = self.started.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = ActiveWrite(self.active.clone());
        assert!(active <= DIRECTORY_STAGE_CONCURRENCY);
        if index + 1 == DIRECTORY_STAGE_CONCURRENCY {
            self.window_full.notify_one();
        }
        // Complete later children before the first child. Its parent must
        // still follow it in the publication queue, including with batch=1.
        if index == 0 {
            self.resume.notified().await;
            if self.fail {
                return Err(std::io::Error::other("injected directory failure").into());
            }
        }
        tokio::task::yield_now().await;
        self.inner.put_slice(data).await
    }
}

#[tokio::test]
async fn directory_staging_is_bounded_ordered_and_cancellable() {
    for (batch_size, count, fail, cancel) in [
        (1, 33, false, false),
        (17, 513, false, false),
        (1, 33, true, false),
        (1, 33, false, true),
    ] {
        let source = tempfile::tempdir().unwrap();
        let mut expected = Directory::new();
        for index in 0..count {
            let name = format!("dir-{index:02}");
            let child_name = format!("child-{index:02}");
            std::fs::create_dir_all(source.path().join(&name).join(&child_name)).unwrap();
            let mut child = Directory::new();
            child
                .add(
                    PathComponent::try_from(child_name.as_str()).unwrap(),
                    Node::Directory {
                        digest: Directory::new().digest(),
                        size: 0,
                    },
                )
                .unwrap();
            expected
                .add(
                    PathComponent::try_from(name.as_str()).unwrap(),
                    Node::Directory {
                        digest: child.digest(),
                        size: child.size(),
                    },
                )
                .unwrap();
        }
        let payloads = PausedDirectories {
            inner: MemoryBlobStore::new(),
            started: Arc::default(),
            active: Arc::default(),
            window_full: Arc::default(),
            resume: Arc::default(),
            fail,
        };
        let repository = Arc::new(Repository::with_formats(
            payloads.clone(),
            OrderedCheckpoints {
                inner: MemoryMetadataStore::new().unwrap(),
                limit: batch_size,
            },
            FormatRegistry::builtin(),
            FormatLimits {
                max_batch_objects: batch_size,
                ..FormatLimits::default()
            },
        ));
        let name = RootName::try_from("directories").unwrap();
        let task = {
            let repository = repository.clone();
            let input = crate::import::FilesystemImport::new(source.path(), name.clone());
            tokio::spawn(async move { repository.import(input).await })
        };
        payloads.window_full.notified().await;
        // Let every unblocked write settle. Ordered buffering must retain
        // at most one window while the first write is paused.
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            payloads.started.load(Ordering::SeqCst),
            DIRECTORY_STAGE_CONCURRENCY
        );
        assert!(
            repository
                .state
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap()
                .is_none()
        );
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            payloads.resume.notify_one();
            let result = task.await.unwrap();
            if fail {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("injected directory failure")
                );
            } else {
                let key = result.unwrap();
                assert_eq!(key, ObjectKey::directory(expected.digest()));
                assert!(matches!(
                    repository.verify_closure(&key).await.unwrap(),
                    ClosureStatus::Complete { .. }
                ));
                let output = tempfile::tempdir().unwrap();
                let target = output.path().join("restored");
                repository.checkout(&key, &target).await.unwrap();
                for index in 0..count {
                    assert!(
                        target
                            .join(format!("dir-{index:02}/child-{index:02}"))
                            .is_dir()
                    );
                }
            }
        }
        assert_eq!(payloads.active.load(Ordering::SeqCst), 0);
        if fail || cancel {
            assert!(
                repository
                    .state
                    .snapshot()
                    .await
                    .unwrap()
                    .root(&name)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        crate::flush_repository_leases().await.unwrap();
    }
}
