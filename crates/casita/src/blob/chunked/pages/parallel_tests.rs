//! Concurrency, ordering, cancellation, and integrity regression tests.
use super::*;
use async_trait::async_trait;
use futures::stream::BoxStream;
use object_store::*;
use std::{
    fmt,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
struct Delayed {
    inner: Arc<dyn ObjectStore>,
    slow: Path,
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: Mutex<Vec<Path>>,
}
impl fmt::Display for Delayed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Delayed")
    }
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl ObjectStore for Delayed {
    async fn put_opts(
        &self,
        path: &Path,
        payload: PutPayload,
        options: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.inner.put_opts(path, payload, options).await
    }
    async fn put_multipart_opts(
        &self,
        path: &Path,
        options: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(path, options).await
    }
    async fn get_opts(&self, path: &Path, options: GetOptions) -> object_store::Result<GetResult> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = Active(&self.active);
        self.peak.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(if *path == self.slow {
            30
        } else {
            1
        }))
        .await;
        let result = self.inner.get_opts(path, options).await;
        self.completed.lock().unwrap().push(path.clone());
        result
    }
    fn delete_stream(
        &self,
        paths: BoxStream<'static, object_store::Result<Path>>,
    ) -> BoxStream<'static, object_store::Result<Path>> {
        self.inner.delete_stream(paths)
    }
    fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.inner.list(prefix)
    }
    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }
    async fn copy_opts(
        &self,
        from: &Path,
        to: &Path,
        options: CopyOptions,
    ) -> object_store::Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
    async fn rename_opts(
        &self,
        from: &Path,
        to: &Path,
        options: RenameOptions,
    ) -> object_store::Result<()> {
        self.inner.rename_opts(from, to, options).await
    }
}

async fn fixture(count: usize) -> (Pages, Root, Vec<ChunkMeta>, Arc<Delayed>) {
    let store = ChunkedBlobStore::new(Arc::new(memory::InMemory::new()), Path::default(), 65536);
    let mut pages = Pages::from(&store);
    let chunks: Vec<_> = (0..count)
        .map(|i| ChunkMeta {
            digest: ChunkId::new(blake3::hash(&i.to_le_bytes()).into()),
            size: 65536 + i as u64,
        })
        .collect();
    let root = pages.build_chunks(&chunks).await.unwrap();
    let mut first = root;
    while let Node::Branch(children) = pages.load(first).await.unwrap() {
        first = children[0];
    }
    let delayed = Arc::new(Delayed {
        inner: pages.objects.clone(),
        slow: pages.path(&first.hash),
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        completed: Mutex::new(Vec::new()),
    });
    pages.objects = delayed.clone();
    (pages, root, chunks, delayed)
}

#[tokio::test]
async fn leaves_overlap_with_bounded_concurrency_and_preserve_order() {
    for count in [64, 65, 511, 512, 513, 4096, 4097] {
        let (pages, root, expected, delayed) = fixture(count).await;
        assert_eq!(pages.collect_chunks(root).await.unwrap(), expected);
        assert_eq!(delayed.active.load(Ordering::SeqCst), 0);
        assert_eq!(
            delayed.peak.load(Ordering::SeqCst),
            count.div_ceil(FANOUT).min(LEAF_READ_CONCURRENCY)
        );
        if count > FANOUT {
            let done = delayed.completed.lock().unwrap();
            // Root/branch requests finish before leaves; the slow first leaf
            // finishes after a later sibling, yet output retains file order.
            let slow = done.iter().position(|path| *path == delayed.slow).unwrap();
            assert!(slow > root.height as usize);
        }
    }
}

#[tokio::test]
async fn failed_parallel_leaf_never_returns_partial_manifest() {
    for missing in [false, true] {
        let (pages, root, _, delayed) = fixture(513).await;
        if missing {
            delayed.inner.delete(&delayed.slow).await.unwrap();
        } else {
            delayed
                .inner
                .put(&delayed.slow, Bytes::from_static(b"corrupt page").into())
                .await
                .unwrap();
        }
        let error = pages.collect_chunks(root).await.unwrap_err();
        assert!(
            error
                .get_ref()
                .unwrap()
                .is::<crate::blob::BlobIntegrityError>()
        );
        assert_eq!(delayed.active.load(Ordering::SeqCst), 0);
        assert!(delayed.peak.load(Ordering::SeqCst) <= LEAF_READ_CONCURRENCY);
    }
}

#[tokio::test]
async fn cancellation_drops_in_flight_page_reads() {
    let (pages, root, _, delayed) = fixture(513).await;
    let mut read = Box::pin(pages.collect_chunks(root));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut read => panic!("traversal finished before cancellation"),
            _ = async {
                while delayed.active.load(Ordering::SeqCst) < LEAF_READ_CONCURRENCY {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    })
    .await
    .unwrap();
    drop(read);
    assert_eq!(delayed.active.load(Ordering::SeqCst), 0);
}
