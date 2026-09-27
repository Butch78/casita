use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::ReadBuf;

use super::*;
use crate::git::fetch::VecAsyncWriter;
use crate::{GitObjectFormat, GitObjectKind, git_object_key_for_body};

fn pack<W>(output: &mut W, limit: usize, side_band_64k: bool) -> PackWriter<'_, W> {
    PackWriter {
        output,
        hasher: Some(gix_hash::hasher(gix_hash::Kind::Sha1)),
        bytes: 0,
        limit,
        trailer_bytes: 20,
        side_band_64k,
        wire_buffer: Vec::new(),
    }
}

fn body(size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    blake3::Hasher::new()
        .update(b"streaming-encoder-tests")
        .finalize_xof()
        .fill(&mut bytes);
    bytes
}

#[tokio::test]
async fn stream_roundtrips_across_buffer_and_output_boundaries() {
    use std::io::Read;
    let permits = Arc::new(Semaphore::new(1));
    for (size, buffer) in [
        (0, 0),
        (37, 1),
        (INPUT_BYTES - 1, INPUT_BYTES),
        (INPUT_BYTES, INPUT_BYTES),
        (INPUT_BYTES + 1, INPUT_BYTES),
        (2 * INPUT_BYTES + 19, usize::MAX),
    ] {
        for side_band in [false, true] {
            let body = body(size);
            let key =
                git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body).unwrap();
            let mut output = VecAsyncWriter::default();
            write_payload(
                &mut body.as_slice(),
                &key,
                size as u64,
                buffer,
                6,
                &permits,
                &mut pack(&mut output, usize::MAX, side_band),
            )
            .await
            .unwrap();
            let wire = output.into_inner();
            let mut compressed = Vec::new();
            if side_band {
                let mut cursor = 0;
                while cursor < wire.len() {
                    let length = usize::from_str_radix(
                        std::str::from_utf8(&wire[cursor..cursor + 4]).unwrap(),
                        16,
                    )
                    .unwrap();
                    assert!(length <= super::super::MAX_PKT_LINE);
                    assert_eq!(wire[cursor + 4], 1);
                    compressed.extend_from_slice(&wire[cursor + 5..cursor + length]);
                    cursor += length;
                }
            } else {
                compressed = wire;
            }
            let mut decoder = flate2::read::ZlibDecoder::new(compressed.as_slice());
            let mut decoded = Vec::new();
            decoder.read_to_end(&mut decoded).unwrap();
            assert_eq!(decoder.total_in() as usize, compressed.len());
            assert_eq!(decoded, body);
            assert_eq!(permits.available_permits(), 1);
        }
    }
}

#[tokio::test]
async fn malformed_payloads_and_output_limits_remain_errors() {
    let body = body(2 * INPUT_BYTES);
    let key = git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body).unwrap();
    let permits = Arc::new(Semaphore::new(1));
    for size in [body.len() - 1, body.len() + 1] {
        let mut output = VecAsyncWriter::default();
        let error = write_payload(
            &mut body.as_slice(),
            &key,
            size as u64,
            INPUT_BYTES,
            6,
            &permits,
            &mut pack(&mut output, usize::MAX, false),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            GitFetchError::Repository(RepositoryError::Metadata(_))
        ));
    }
    let mut output = VecAsyncWriter::default();
    let error = write_payload(
        &mut body.as_slice(),
        &key,
        body.len() as u64,
        INPUT_BYTES,
        6,
        &permits,
        &mut pack(&mut output, 64, false),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, GitFetchError::Limit(_)));
    assert_eq!(permits.available_permits(), 1);
}

#[test]
fn async_file_reads_work_with_one_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let body = body(2 * INPUT_BYTES + 1);
    let path = directory.path().join("payload");
    std::fs::write(&path, &body).unwrap();
    let result = runtime.block_on(async {
        let key =
            git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body).unwrap();
        let mut file = tokio::fs::File::open(path).await.unwrap();
        let mut output = VecAsyncWriter::default();
        tokio::time::timeout(
            Duration::from_secs(5),
            write_payload(
                &mut file,
                &key,
                body.len() as u64,
                INPUT_BYTES,
                6,
                &Arc::new(Semaphore::new(1)),
                &mut pack(&mut output, usize::MAX, false),
            ),
        )
        .await
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    result.unwrap().unwrap();
}

struct CountedReader {
    body: Vec<u8>,
    read: Arc<AtomicUsize>,
}

impl AsyncRead for CountedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        assert!(buf.remaining() <= READ_BYTES);
        let offset = self.read.load(Ordering::SeqCst);
        let length = buf.remaining().min(self.body.len() - offset);
        buf.put_slice(&self.body[offset..offset + length]);
        self.read.fetch_add(length, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
}

struct StalledWriter(Arc<tokio::sync::Notify>);
impl AsyncWrite for StalledWriter {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        self.0.notify_one();
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn backpressure_bounds_read_ahead_and_cancellation_releases_admission() {
    let body = body(4 * INPUT_BYTES);
    let key = git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, &body).unwrap();
    let read = Arc::new(AtomicUsize::new(0));
    let stalled = Arc::new(tokio::sync::Notify::new());
    let permits = Arc::new(Semaphore::new(1));
    let mut reader = CountedReader {
        body,
        read: read.clone(),
    };
    let mut output = StalledWriter(stalled.clone());
    let worker_permits = permits.clone();
    let task = tokio::spawn(async move {
        write_payload(
            &mut reader,
            &key,
            (4 * INPUT_BYTES) as u64,
            usize::MAX,
            6,
            &worker_permits,
            &mut pack(&mut output, usize::MAX, false),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), stalled.notified())
        .await
        .unwrap();
    assert!(read.load(Ordering::SeqCst) <= INPUT_BYTES);
    // Output backpressure must not occupy a CPU permit or a blocking thread.
    assert_eq!(permits.available_permits(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(read.load(Ordering::SeqCst) <= INPUT_BYTES);
}

#[tokio::test]
async fn cancelling_a_running_job_keeps_its_permit_until_completion() {
    let permits = Arc::new(Semaphore::new(1));
    let permit = permits.clone().acquire_owned().await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let job = BlockingJob(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        started.send(()).unwrap();
        wait.recv().unwrap();
    }));
    ready.await.unwrap();
    drop(job);
    assert_eq!(permits.available_permits(), 0);
    release.send(()).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(5), permits.acquire())
        .await
        .unwrap()
        .unwrap();
    drop(permit);
    assert_eq!(permits.available_permits(), 1);
}
