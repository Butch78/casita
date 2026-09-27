//! Object-store byte accounting for reproducible metadata-scaling benchmarks.
use async_trait::async_trait;
use futures::stream::BoxStream;
use object_store::{ObjectStore, path::Path, *};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Default)]
pub struct Counts {
    pub payload_read: AtomicU64,
    pub metadata_read: AtomicU64,
    pub payload_write: AtomicU64,
    pub metadata_write: AtomicU64,
}
impl Counts {
    pub fn reset(&self) {
        for counter in [
            &self.payload_read,
            &self.metadata_read,
            &self.payload_write,
            &self.metadata_write,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
    pub fn values(&self) -> [u64; 4] {
        [
            &self.payload_read,
            &self.metadata_read,
            &self.payload_write,
            &self.metadata_write,
        ]
        .map(|counter| counter.load(Ordering::Relaxed))
    }
}
#[derive(Debug)]
pub struct Counted {
    pub inner: Arc<dyn ObjectStore>,
    pub counts: Arc<Counts>,
}
impl fmt::Display for Counted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Counted({})", self.inner)
    }
}
#[async_trait]
impl ObjectStore for Counted {
    async fn put_opts(
        &self,
        path: &Path,
        payload: PutPayload,
        options: PutOptions,
    ) -> Result<PutResult> {
        let counter = if path.as_ref().starts_with("chunks/") {
            &self.counts.payload_write
        } else {
            &self.counts.metadata_write
        };
        counter.fetch_add(payload.content_length() as u64, Ordering::Relaxed);
        self.inner.put_opts(path, payload, options).await
    }
    async fn put_multipart_opts(
        &self,
        path: &Path,
        options: PutMultipartOptions,
    ) -> Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(path, options).await
    }
    async fn get_opts(&self, path: &Path, options: GetOptions) -> Result<GetResult> {
        let head = options.head;
        let result = self.inner.get_opts(path, options).await?;
        if !head {
            let counter = if path.as_ref().starts_with("chunks/") {
                &self.counts.payload_read
            } else {
                &self.counts.metadata_read
            };
            counter.fetch_add(result.range.end - result.range.start, Ordering::Relaxed);
        }
        Ok(result)
    }
    fn delete_stream(
        &self,
        paths: BoxStream<'static, Result<Path>>,
    ) -> BoxStream<'static, Result<Path>> {
        self.inner.delete_stream(paths)
    }
    fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
        self.inner.list(prefix)
    }
    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }
    async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
    async fn rename_opts(&self, from: &Path, to: &Path, options: RenameOptions) -> Result<()> {
        self.inner.rename_opts(from, to, options).await
    }
}
