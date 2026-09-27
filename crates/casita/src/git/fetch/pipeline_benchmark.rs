//! Local simulation: no sockets, cache, or CPU admission checks.
use super::*;
use crate::blob::{BlobBatchGuard, BlobReader, BlobWriter};
use crate::error::Error;
use crate::metadata::DataPinLease;
use crate::{
    CanonicalRefName, GitRefValue, MemoryBlobStore, MemoryMetadataStore, git_object_key_for_body,
    publish_git_view,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

#[derive(Clone)]
struct DelayedStore {
    inner: MemoryBlobStore,
    permits: Arc<tokio::sync::Semaphore>,
    delay_ms: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

struct Lease {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Reader {
    inner: Box<dyn BlobReader>,
    _lease: Lease,
}
impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncSeek for Reader {
    fn start_seek(mut self: Pin<&mut Self>, position: std::io::SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.inner).start_seek(position)
    }
    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.inner).poll_complete(cx)
    }
}
impl BlobReader for Reader {}

#[async_trait::async_trait]
impl BlobStore for DelayedStore {
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }
    fn begin_pinned_batch(&self, pin: DataPinLease) -> Result<BlobBatchGuard, Error> {
        self.inner.begin_pinned_batch(pin)
    }
    async fn has(&self, digest: &crate::BlobId) -> Result<bool, Error> {
        self.inner.has(digest).await
    }
    async fn open_write(&self) -> Box<dyn BlobWriter> {
        self.inner.open_write().await
    }
    async fn open_read(
        &self,
        digest: &crate::BlobId,
    ) -> Result<Option<Box<dyn BlobReader>>, Error> {
        let permit = self.permits.clone().acquire_owned().await.unwrap();
        let lease = Lease {
            _permit: permit,
            active: self.active.clone(),
        };
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.reads.fetch_add(1, Ordering::SeqCst);
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        }
        Ok(self.inner.open_read(digest).await?.map(|inner| {
            Box::new(Reader {
                inner,
                _lease: lease,
            }) as Box<dyn BlobReader>
        }))
    }
    async fn open_read_scoped(
        &self,
        digest: &crate::BlobId,
        _pin: DataPinLease,
        _catalog: Option<&[u8]>,
    ) -> Result<Option<Box<dyn BlobReader>>, Error> {
        self.open_read(digest).await
    }
}

struct Sink {
    bytes: Vec<u8>,
    delay: Duration,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    writes: usize,
}
impl AsyncWrite for Sink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if !self.delay.is_zero() {
            if self.sleep.is_none() {
                self.sleep = Some(Box::pin(tokio::time::sleep(self.delay)));
            }
            if self.sleep.as_mut().unwrap().as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.sleep = None;
        }
        let count = bytes.len().min(64 * 1024);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.writes += 1;
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// Independent native pack decoding; fixture contains only whole blob entries.
fn validate(bytes: &[u8], bodies: &BTreeMap<Vec<u8>, Vec<u8>>) {
    use std::io::Read;
    assert_eq!(&bytes[..8], b"PACK\0\0\0\x02");
    assert_eq!(
        u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize,
        bodies.len()
    );
    let end = bytes.len() - 20;
    let mut hash = gix_hash::hasher(gix_hash::Kind::Sha1);
    hash.update(&bytes[..end]);
    assert_eq!(hash.try_finalize().unwrap().as_slice(), &bytes[end..]);
    let mut offset = 12;
    let mut seen = BTreeSet::new();
    for _ in bodies {
        let first = bytes[offset];
        offset += 1;
        assert_eq!((first >> 4) & 7, 3);
        let mut size = (first & 15) as usize;
        let mut shift = 4;
        let mut byte = first;
        while byte & 128 != 0 {
            byte = bytes[offset];
            offset += 1;
            size |= ((byte & 127) as usize) << shift;
            shift += 7;
        }
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[offset..end]);
        let mut body = Vec::new();
        decoder.read_to_end(&mut body).unwrap();
        assert_eq!(body.len(), size);
        offset += decoder.total_in() as usize;
        let key =
            git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body).unwrap();
        assert_eq!(bodies.get(key.native_id()), Some(&body));
        assert!(seen.insert(key.native_id().to_vec()));
    }
    assert_eq!(offset, end);
    assert_eq!(seen.len(), bodies.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "permanent performance corpus; run benchmark git-pack-delayed"]
async fn controlled_reads_and_backpressure() {
    let repetitions: usize = std::env::var("CASITA_PIPELINE_REPETITIONS")
        .unwrap_or_else(|_| "6".into())
        .parse()
        .unwrap();
    assert!(repetitions > 0);
    let isolation = std::env::var("CASITA_PIPELINE_ISOLATION").as_deref() == Ok("1");
    let corpora: &[&str] = if isolation {
        &["small", "boundary", "buffered", "below", "at", "above"]
    } else {
        &["small", "boundary"]
    };
    let permit_counts: &[usize] = if isolation { &[1] } else { &[1, 2, 8] };
    let levels: &[u32] = if isolation { &[0, 6] } else { &[6] };
    let read_delays: &[usize] = if isolation { &[0] } else { &[0, 5] };
    let output_delays: &[u64] = if isolation { &[0] } else { &[0, 2] };
    for &corpus in corpora {
        for &permits in permit_counts {
            for &compression_level in levels {
                let store = DelayedStore {
                    inner: MemoryBlobStore::default(),
                    permits: Arc::new(tokio::sync::Semaphore::new(permits)),
                    delay_ms: Arc::new(AtomicUsize::new(0)),
                    active: Arc::new(AtomicUsize::new(0)),
                    peak: Arc::new(AtomicUsize::new(0)),
                    reads: Arc::new(AtomicUsize::new(0)),
                };
                let repository =
                    Repository::new(store.clone(), MemoryMetadataStore::new().unwrap());
                let mutation = repository.mutation_session().await.unwrap();
                let mut staged = Vec::new();
                let mut objects = BTreeSet::new();
                let mut bodies = BTreeMap::new();
                let mut refs = BTreeMap::new();
                let sizes: Vec<_> = match corpus {
                    "small" => vec![64 * 1024; 24],
                    "boundary" => [vec![64 * 1024; 16], vec![1048575, 1048576, 1048577]].concat(),
                    "buffered" => [vec![64 * 1024; 16], vec![1048575, 1048576]].concat(),
                    "below" => vec![1048575],
                    "at" => vec![1048576],
                    "above" => vec![1048577],
                    _ => unreachable!(),
                };
                let mut state = 9u64;
                for (index, size) in sizes.into_iter().enumerate() {
                    let body: Vec<u8> = (0..size)
                        .map(|_| {
                            state ^= state << 13;
                            state ^= state >> 7;
                            state ^= state << 17;
                            state as u8
                        })
                        .collect();
                    let key =
                        git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body)
                            .unwrap();
                    staged.push(mutation.stage_object(key.clone(), &body).await.unwrap());
                    bodies.insert(key.native_id().to_vec(), body);
                    objects.insert(key.clone());
                    refs.insert(
                        CanonicalRefName::try_from(format!("refs/tags/blob-{index}").as_str())
                            .unwrap(),
                        GitRefValue::Direct(key),
                    );
                }
                mutation.publish_unrooted(staged).await.unwrap();
                drop(mutation);
                publish_git_view(
                    &repository,
                    "delayed",
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
                let limits = GitFetchLimits {
                    compression_level,
                    ..GitFetchLimits::default()
                };
                let service = GitFetchService::bind(&repository, "delayed", limits)
                    .await
                    .unwrap();
                let selected: BTreeSet<u32> = (0..service.inner.records.len() as u32).collect();
                let mut baseline = VecAsyncWriter::default();
                service
                    .write_prepared_pack(selected.clone(), &mut baseline, false, false)
                    .await
                    .unwrap();
                let baseline = baseline.into_inner();
                validate(&baseline, &bodies);
                for &read_ms in read_delays {
                    for &output_ms in output_delays {
                        store.delay_ms.store(read_ms, Ordering::SeqCst);
                        // One warmup per mode, then three rotated rounds and their reverses.
                        for round in 0..=repetitions {
                            let mut modes = ["serial", "batch-8", "pipeline-8"];
                            let repetition = round.saturating_sub(1);
                            modes.rotate_left(repetition % 3);
                            if repetition / 3 % 2 == 1 {
                                modes.reverse();
                            }
                            for mode in modes {
                                store.peak.store(0, Ordering::SeqCst);
                                store.reads.store(0, Ordering::SeqCst);
                                let mut sink = Sink {
                                    bytes: Vec::new(),
                                    delay: Duration::from_millis(output_ms),
                                    sleep: None,
                                    writes: 0,
                                };
                                let start = Instant::now();
                                tokio::time::timeout(
                                    Duration::from_secs(30),
                                    service.write_prepared_pack_with_read_batch(
                                        selected.clone(),
                                        &mut sink,
                                        false,
                                        false,
                                        if mode == "serial" { 1 } else { 8 },
                                        mode == "pipeline-8",
                                    ),
                                )
                                .await
                                .expect("pack generation deadlocked or exceeded 30 seconds")
                                .unwrap();
                                let seconds = start.elapsed().as_secs_f64();
                                assert_eq!(store.active.load(Ordering::SeqCst), 0);
                                assert_eq!(store.permits.available_permits(), permits);
                                let peak = store.peak.load(Ordering::SeqCst);
                                assert!(
                                    peak > 0
                                        && peak
                                            <= permits.min(if mode == "serial" { 1 } else { 8 })
                                );
                                assert_eq!(store.reads.load(Ordering::SeqCst), bodies.len());
                                assert_eq!(sink.bytes, baseline);
                                validate(&sink.bytes, &bodies);
                                println!(
                                    "PIPELINE_SAMPLE {}",
                                    serde_json::json!({"corpus":corpus,"compression_level":compression_level,"permits":permits,"read_ms":read_ms,"output_ms":output_ms,"mode":mode,"phase":if round==0 {"warmup"} else {"measured"},"repetition":repetition,"seconds":seconds,"peak_readers":peak,"payload_reads":bodies.len(),"objects":bodies.len(),"output_writes":sink.writes,"pack_blake3":blake3::hash(&sink.bytes).to_hex().to_string(),"correctness":"passed"})
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
