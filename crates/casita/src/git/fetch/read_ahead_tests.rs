use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;
use crate::blob::{BlobBatchGuard, BlobReader, BlobWriter};
use crate::error::Error;
use crate::metadata::DataPinLease;
use crate::{
    BlobId, CanonicalRefName, GitRefValue, MemoryBlobStore, MemoryMetadataStore,
    git_object_key_for_body, publish_git_view,
};

#[derive(Clone)]
struct GatedStore {
    inner: MemoryBlobStore,
    stalled: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    started: Arc<AtomicUsize>,
    permits: Arc<tokio::sync::Semaphore>,
}

impl Default for GatedStore {
    fn default() -> Self {
        Self {
            inner: MemoryBlobStore::default(),
            stalled: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(AtomicUsize::new(0)),
            permits: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

struct ActiveRead<'a>(&'a AtomicUsize);

impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl BlobStore for GatedStore {
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }

    fn begin_pinned_batch(&self, pin: DataPinLease) -> Result<BlobBatchGuard, Error> {
        self.inner.begin_pinned_batch(pin)
    }

    async fn has(&self, digest: &BlobId) -> Result<bool, Error> {
        self.inner.has(digest).await
    }

    async fn open_write(&self) -> Box<dyn BlobWriter> {
        self.inner.open_write().await
    }

    async fn open_read(&self, digest: &BlobId) -> Result<Option<Box<dyn BlobReader>>, Error> {
        if self.stalled.load(Ordering::SeqCst) {
            self.active.fetch_add(1, Ordering::SeqCst);
            self.started.fetch_add(1, Ordering::SeqCst);
            let _active = ActiveRead(&self.active);
            self.permits.acquire().await.unwrap().forget();
        }
        self.inner.open_read(digest).await
    }

    async fn open_read_scoped(
        &self,
        digest: &BlobId,
        _pin: DataPinLease,
        _catalog: Option<&[u8]>,
    ) -> Result<Option<Box<dyn BlobReader>>, Error> {
        self.open_read(digest).await
    }
}

async fn fixture(with_large: bool) -> Repository<GatedStore, MemoryMetadataStore> {
    let repository = Repository::new(GatedStore::default(), MemoryMetadataStore::new().unwrap());
    let mutation = repository.mutation_session().await.unwrap();
    let mut staged = Vec::new();
    let mut objects = BTreeSet::new();
    let mut refs = BTreeMap::new();
    for index in 0..(24 + usize::from(with_large)) {
        let body = if index == 24 {
            "x".repeat(MAX_BUFFERED_PACK_OBJECT_BYTES as usize + 1)
        } else {
            format!("payload {index}")
        };
        let key =
            git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, body.as_bytes())
                .unwrap();
        staged.push(
            mutation
                .stage_object(key.clone(), body.as_bytes())
                .await
                .unwrap(),
        );
        objects.insert(key.clone());
        refs.insert(
            CanonicalRefName::try_from(format!("refs/tags/blob-{index}").as_str()).unwrap(),
            GitRefValue::Direct(key),
        );
    }
    mutation.publish_unrooted(staged).await.unwrap();
    drop(mutation);
    publish_git_view(
        &repository,
        "read-ahead",
        &GitViewBody {
            object_format: GitObjectFormat::Sha1,
            refs,
            default_ref: None,
            pack: None,
            objects,
        },
    )
    .await
    .unwrap();
    repository
}

#[tokio::test]
async fn stalled_reads_overlap_are_bounded_and_cancel_with_the_pack() {
    let repository = fixture(false).await;
    let service = GitFetchService::bind(&repository, "read-ahead", GitFetchLimits::default())
        .await
        .unwrap();
    let selected = (0..service.inner.records.len() as u32).collect();
    repository.payloads().stalled.store(true, Ordering::SeqCst);
    let mut output = VecAsyncWriter::default();
    let mut fetch = Box::pin(service.write_prepared_pack(selected, &mut output, false, false));
    assert!(futures::poll!(&mut fetch).is_pending());
    // A serial reader would admit only one; an unbounded reader would admit 24.
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 8);
    assert!(futures::poll!(&mut fetch).is_pending());
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 8);
    drop(fetch);
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 0);
    assert_eq!(output.into_inner().len(), 12); // only the pack header escaped
}

#[tokio::test]
async fn read_ahead_stops_before_a_streaming_object() {
    let repository = fixture(true).await;
    let service = GitFetchService::bind(&repository, "read-ahead", GitFetchLimits::default())
        .await
        .unwrap();
    let large = (0..service.inner.records.len() as u32)
        .find(|index| service.catalog_record(*index).payload_size > MAX_BUFFERED_PACK_OBJECT_BYTES)
        .unwrap();
    assert!(large > 0 && large + 1 < service.inner.records.len() as u32);
    // One stalled small read, then a large object, then more small reads.
    // Starting the latter could retain backend permits needed by the large
    // object's stream, which would prevent either path from making progress.
    let selected = (large - 1..service.inner.records.len() as u32).collect();
    repository.payloads().stalled.store(true, Ordering::SeqCst);
    let mut output = VecAsyncWriter::default();
    let mut fetch = Box::pin(service.write_prepared_pack(selected, &mut output, false, false));
    assert!(futures::poll!(&mut fetch).is_pending());
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 1);
    drop(fetch);
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 0);
}

#[cfg(feature = "experimental")]
#[tokio::test]
async fn benchmark_read_modes_preserve_pack_bytes_and_reject_invalid_bounds() {
    let repository = fixture(true).await;
    let service = GitFetchService::bind(&repository, "read-ahead", GitFetchLimits::default())
        .await
        .unwrap();
    let request = GitFetchRequest {
        wants: (0..service.inner.records.len() as u32)
            .map(|index| service.catalog_record(index).key.native_id().to_vec())
            .collect(),
        haves: Vec::new(),
        depth: None,
        done: true,
        multi_ack_detailed: false,
        side_band_64k: false,
    };
    let production = service.build_pack(&request).await.unwrap();
    for batch in [1, 2, 4, 8] {
        let measured = service.benchmark_build_pack(&request, batch).await.unwrap();
        assert_eq!(measured.pack, production.pack);
        let pipelined = service
            .benchmark_build_pack_pipelined(&request, batch)
            .await
            .unwrap();
        assert_eq!(pipelined.pack, production.pack);
    }
    for batch in [0, 9] {
        assert!(matches!(
            service.benchmark_build_pack(&request, batch).await,
            Err(GitFetchError::Limit(_))
        ));
        assert!(matches!(
            service
                .benchmark_build_pack_pipelined(&request, batch)
                .await,
            Err(GitFetchError::Limit(_))
        ));
    }
}

struct StallAfterFirstWindow(Arc<AtomicUsize>);

impl AsyncWrite for StallAfterFirstWindow {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Allow output until the second read window is admitted, independent
        // of the number of encoder workers available on the test machine.
        if self.0.load(Ordering::SeqCst) >= 16 {
            return Poll::Pending;
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn pipeline_drains_admitted_reads_under_backpressure_without_reading_the_next_window() {
    let repository = fixture(false).await;
    let service = GitFetchService::bind(&repository, "read-ahead", GitFetchLimits::default())
        .await
        .unwrap();
    let store = repository.payloads();
    store.stalled.store(true, Ordering::SeqCst);
    let mut output = StallAfterFirstWindow(Arc::clone(&store.started));
    let selected = (0..service.inner.records.len() as u32).collect();
    let mut fetch = Box::pin(service.write_prepared_pack_with_read_batch(
        selected,
        &mut output,
        false,
        false,
        8,
        true,
    ));
    assert!(futures::poll!(&mut fetch).is_pending());
    assert_eq!(store.active.load(Ordering::SeqCst), 8);
    store.permits.add_permits(8);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.started.load(Ordering::SeqCst) < 16 {
            assert!(futures::poll!(&mut fetch).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // First object in the second window starts encoding. Output stalls with
    // the other seven storage reads still pending.
    store.permits.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.active.load(Ordering::SeqCst) != 7 {
            assert!(futures::poll!(&mut fetch).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.active.load(Ordering::SeqCst), 7);
    store.permits.add_permits(7);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.active.load(Ordering::SeqCst) != 0 {
            assert!(futures::poll!(&mut fetch).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Finishing those reads must release storage resources even though output
    // remains blocked. A third window would violate the admission bound.
    assert!(futures::poll!(&mut fetch).is_pending());
    assert_eq!(store.started.load(Ordering::SeqCst), 16);
    drop(fetch);
    assert_eq!(store.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pipeline_cancellation_drops_reads_and_stops_at_streaming_boundaries() {
    let repository = fixture(true).await;
    let service = GitFetchService::bind(&repository, "read-ahead", GitFetchLimits::default())
        .await
        .unwrap();
    let large = (0..service.inner.records.len() as u32)
        .find(|index| service.catalog_record(*index).payload_size > MAX_BUFFERED_PACK_OBJECT_BYTES)
        .unwrap();
    assert!(large > 0 && large + 1 < service.inner.records.len() as u32);
    let selected = (large - 1..service.inner.records.len() as u32).collect();
    repository.payloads().stalled.store(true, Ordering::SeqCst);
    let mut output = VecAsyncWriter::default();
    let mut fetch = Box::pin(service.write_prepared_pack_with_read_batch(
        selected,
        &mut output,
        false,
        false,
        8,
        true,
    ));
    assert!(futures::poll!(&mut fetch).is_pending());
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 1);
    drop(fetch);
    assert_eq!(repository.payloads().active.load(Ordering::SeqCst), 0);
}
