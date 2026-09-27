//! Sliced payload transfer: codec throughput and wire bytes.
//!
//! A rebuilt blob differs from its base in a 32-byte hash every `spacing`
//! bytes. The codec cases measure indexing the base, encoding the rebuilt blob
//! against it, and decoding the frame back; every timed iteration is checked
//! against the original bytes outside the timer. The sync cases move the same
//! blobs through the stdio transfer protocol between two in-memory
//! repositories and count the bytes the server writes, first with an empty
//! destination and then when the destination already holds the base under
//! the root being replaced. Spacings bracket the cliff below the 1 KiB
//! discovery chunk, where nothing can be sliced.
//!
//! Set `CASITA_SLICED_REPORT` to write every emitted JSON row to a file.

mod bench_util;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use casita::experimental::{
    BlobId, BlobSliceSources, BlobStore, ChunkedBlobStore, ClosureStatus, DEFAULT_AVG_CHUNK_SIZE,
    DestinationRoot, Digest, HeldSession, MemoryBlobStore, MemoryMetadataStore, ObjectKey,
    ObjectRequest, Repository, RootChange, RootName, SliceIndex, TransferOptions,
    TransferReadSession, TransferRequest, TransferResult, TransferSelection,
    connect_transfer_stdio_source, decode_sliced, encode_sliced, encode_sliced_stream,
    serve_transfer_stdio, transfer,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt};

const MIB: usize = 1024 * 1024;
const BLOB_BYTES: usize = 8 * MIB;
/// Below the 1 KiB discovery chunk every chunk contains an edit; above it
/// the run between edits is one copy.
const SPACINGS: [usize; 3] = [512, 64 * 1024, MIB];
const SCHEMA: &str = "casita.sliced-transfer.v1";

fn human(bytes: usize) -> String {
    if bytes.is_multiple_of(MIB) {
        format!("{}MiB", bytes / MIB)
    } else if bytes.is_multiple_of(1024) {
        format!("{}KiB", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

/// The same bytes with a different 32-byte hash every `spacing` bytes.
fn rebuilt(base: &[u8], spacing: usize) -> Vec<u8> {
    let mut bytes = base.to_vec();
    let mut position = spacing / 2;
    while position + 32 <= bytes.len() {
        for byte in &mut bytes[position..position + 32] {
            *byte = byte.wrapping_add(1);
        }
        position += spacing;
    }
    bytes
}

fn id_of(bytes: &[u8]) -> BlobId {
    BlobId::new(Digest::from(*blake3::hash(bytes).as_bytes()))
}

async fn store(bytes: &[u8]) -> Arc<MemoryBlobStore> {
    let store = MemoryBlobStore::new();
    let mut writer = store.open_write().await;
    writer.write_all(bytes).await.unwrap();
    writer.close().await.unwrap();
    Arc::new(store)
}

async fn index(base: &[u8]) -> SliceIndex {
    let mut index = SliceIndex::new(1 << 22);
    index
        .index_blob(id_of(base), std::io::Cursor::new(base))
        .await
        .unwrap();
    index
}

fn codec(c: &mut Criterion, report: &mut Vec<Value>) {
    let rt = bench_util::runtime();
    let base = bench_util::random_bytes(31, BLOB_BYTES);
    let base_store = rt.block_on(store(&base));
    let sender = BlobSliceSources::new(base_store.clone());
    let receiver = BlobSliceSources::new(base_store);
    let mut group = c.benchmark_group("sliced_codec");
    group.sample_size(10);

    group.throughput(Throughput::Bytes(BLOB_BYTES as u64));
    group.bench_function("index", |b| {
        b.to_async(&rt).iter(|| async {
            let built = index(&base).await;
            assert!(built.entries() > 0);
            built
        })
    });
    let built = rt.block_on(index(&base));

    for spacing in SPACINGS {
        let new = rebuilt(&base, spacing);
        let new_id = id_of(&new);
        let (frame, stats) = rt
            .block_on(encode_sliced(new_id, &new, &built, &sender))
            .unwrap();
        let mut output = Vec::new();
        let decoded = rt
            .block_on(decode_sliced(
                &mut std::io::Cursor::new(&frame),
                new_id,
                new.len() as u64,
                &receiver,
                &mut output,
            ))
            .unwrap();
        assert_eq!(output, new);
        assert_eq!(decoded.copy_bytes, stats.copy_bytes);
        assert_eq!(decoded.literal_bytes, stats.literal_bytes);
        let row = json!({
            "schema": SCHEMA, "case": "codec", "spacing_bytes": spacing,
            "blob_bytes": BLOB_BYTES, "frame_bytes": stats.frame_bytes,
            "frame_percent": stats.frame_bytes as f64 * 100.0 / BLOB_BYTES as f64,
            "copies": stats.copies, "literals": stats.literals,
            "copy_bytes": stats.copy_bytes, "literal_bytes": stats.literal_bytes,
            "sources": stats.sources, "index_entries": built.entries(),
            "correctness": "passed",
        });
        eprintln!("{row}");
        report.push(row);

        group.throughput(Throughput::Bytes(BLOB_BYTES as u64));
        group.bench_function(BenchmarkId::new("encode", human(spacing)), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let (new, frame, built, sender) = (&new, &frame, &built, &sender);
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let started = Instant::now();
                        let (encoded, _) = encode_sliced(new_id, new, built, sender).await.unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(encoded.len(), frame.len());
                    }
                    elapsed
                }
            })
        });
        group.bench_function(BenchmarkId::new("decode", human(spacing)), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let (new, frame, receiver) = (&new, &frame, &receiver);
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut output = Vec::with_capacity(new.len());
                        let started = Instant::now();
                        decode_sliced(
                            &mut std::io::Cursor::new(frame),
                            new_id,
                            new.len() as u64,
                            receiver,
                            &mut output,
                        )
                        .await
                        .unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(&output, new);
                    }
                    elapsed
                }
            })
        });
    }
    group.finish();
}

/// Counts the bytes a server writes onto the wire.
struct CountingWriter<W> {
    inner: W,
    bytes: Arc<AtomicU64>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for CountingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let written = Pin::new(&mut self.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(count)) = &written {
            self.bytes.fetch_add(*count as u64, Ordering::Relaxed);
        }
        written
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

type Memory = Repository<ChunkedBlobStore, MemoryMetadataStore>;

fn repository() -> Memory {
    Repository::new(
        bench_util::memory_store(DEFAULT_AVG_CHUNK_SIZE).0,
        MemoryMetadataStore::new().unwrap(),
    )
}

async fn publish(repository: &Memory, name: &RootName, bytes: &[u8]) -> ObjectKey {
    let mutation = repository.mutation_session().await.unwrap();
    let staged = mutation.stage_blob(bytes).await.unwrap();
    let key = staged.record().key().clone();
    mutation
        .publish(
            vec![staged],
            vec![RootChange::Set {
                name: name.clone(),
                target: key.clone(),
            }],
        )
        .await
        .unwrap();
    key
}

/// One stdio sync of `target` under `name`: the result, the bytes the server
/// wrote, and the elapsed time from connect to a verified destination.
async fn sync(
    source: Arc<Memory>,
    destination: &Memory,
    name: &RootName,
    target: &ObjectKey,
) -> (TransferResult, u64, Duration, u64) {
    let started = Instant::now();
    let (client, server) = tokio::io::duplex(256 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let bytes = Arc::new(AtomicU64::new(0));
    let counting = CountingWriter {
        inner: server_write,
        bytes: bytes.clone(),
    };
    let server =
        tokio::spawn(async move { serve_transfer_stdio(&*source, server_read, counting).await });
    let session =
        connect_transfer_stdio_source(client_read, client_write, TransferSelection::Snapshot)
            .await
            .unwrap();
    let result = transfer(
        &HeldSession(&session),
        destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: target.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: name.clone(),
                target: target.clone(),
            }],
        },
        TransferOptions::default(),
    )
    .await
    .unwrap();
    let requests = session.transport_requests().unwrap_or(0);
    drop(session);
    server.await.unwrap().unwrap();
    assert!(matches!(
        destination.verify_closure(target).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    (
        result,
        bytes.load(Ordering::Relaxed),
        started.elapsed(),
        requests,
    )
}

fn transport(c: &mut Criterion, report: &mut Vec<Value>) {
    let rt = bench_util::runtime();
    let base = bench_util::random_bytes(32, BLOB_BYTES);
    let name = RootName::try_from("system").unwrap();
    let mut group = c.benchmark_group("sliced_sync");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(BLOB_BYTES as u64));

    for spacing in SPACINGS {
        let new = rebuilt(&base, spacing);
        // One gated run records wire bytes and progress for both phases.
        let (cold, rebuild) = rt.block_on(async {
            let source = Arc::new(repository());
            let base_key = publish(&source, &name, &base).await;
            let destination = repository();
            let cold = sync(source.clone(), &destination, &name, &base_key).await;
            let new_key = publish(&source, &name, &new).await;
            let rebuild = sync(source, &destination, &name, &new_key).await;
            (cold, rebuild)
        });
        assert_eq!(cold.0.progress.slice_literal_bytes, BLOB_BYTES as u64);
        assert_eq!(cold.0.progress.slice_copy_bytes, 0);
        if spacing > 2048 {
            assert!(
                rebuild.0.progress.slice_copy_bytes >= BLOB_BYTES as u64 * 95 / 100,
                "{:?}",
                rebuild.0.progress
            );
            assert!(
                rebuild.1 * 20 < cold.1,
                "wire {} against {}",
                rebuild.1,
                cold.1
            );
        } else {
            assert_eq!(rebuild.0.progress.slice_copy_bytes, 0);
        }
        for (phase, (result, wire, elapsed, requests)) in [("cold", &cold), ("rebuild", &rebuild)] {
            let row = json!({
                "schema": SCHEMA, "case": "sync", "phase": phase, "spacing_bytes": spacing,
                "blob_bytes": BLOB_BYTES, "wire_bytes": wire,
                "wire_percent": *wire as f64 * 100.0 / BLOB_BYTES as f64,
                "slice_copy_bytes": result.progress.slice_copy_bytes,
                "slice_literal_bytes": result.progress.slice_literal_bytes,
                "payloads_sent": result.progress.payloads_sent,
                "transport_requests": requests,
                "wall_seconds": elapsed.as_secs_f64(),
                "correctness": "passed",
            });
            eprintln!("{row}");
            report.push(row);
        }

        group.bench_function(BenchmarkId::new("rebuild", human(spacing)), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let name = name.clone();
                let base = base.clone();
                let new = new.clone();
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let source = Arc::new(repository());
                        let base_key = publish(&source, &name, &base).await;
                        let destination = repository();
                        sync(source.clone(), &destination, &name, &base_key).await;
                        let new_key = publish(&source, &name, &new).await;
                        let (_, _, took, _) = sync(source, &destination, &name, &new_key).await;
                        elapsed += took;
                    }
                    elapsed
                }
            })
        });
    }
    group.finish();
}

/// Where the time goes on each side, against the real chunked backend: the
/// server encodes a rebuilt blob through slice sources over a chunked store,
/// the receiver decodes the frame to a sink (base reads only) and through the
/// chunked writer (base reads plus re-chunking, hashing and compression), and
/// a plain write of the same bytes is the storage cost without any transfer.
fn costs(c: &mut Criterion, report: &mut Vec<Value>) {
    let rt = bench_util::runtime();
    let base = bench_util::random_bytes(33, BLOB_BYTES);
    let (store, _) = bench_util::memory_store(DEFAULT_AVG_CHUNK_SIZE);
    let store = Arc::new(store);
    let base_id = rt.block_on(bench_util::write_blob(&*store, &base));
    let sources = BlobSliceSources::new(store.clone());
    let mut group = c.benchmark_group("sliced_cost");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(BLOB_BYTES as u64));

    group.bench_function("index_chunked", |b| {
        b.to_async(&rt).iter(|| async {
            let mut index = SliceIndex::new(1 << 22);
            let reader = store.open_read(&base_id).await.unwrap().unwrap();
            index.index_blob(base_id, reader).await.unwrap();
            assert!(index.entries() > 0);
            index
        })
    });
    let index = rt.block_on(async {
        let mut index = SliceIndex::new(1 << 22);
        let reader = store.open_read(&base_id).await.unwrap().unwrap();
        index.index_blob(base_id, reader).await.unwrap();
        index
    });
    group.bench_function("write_plaintext", |b| {
        b.to_async(&rt).iter(|| async {
            let id = bench_util::write_blob(&*store, &base).await;
            assert_eq!(id, base_id);
        })
    });

    for spacing in SPACINGS {
        let new = rebuilt(&base, spacing);
        let new_id = id_of(&new);
        let mut frame = Vec::new();
        let stats = rt
            .block_on(encode_sliced_stream(
                new_id,
                new.len() as u64,
                std::io::Cursor::new(&new),
                &index,
                &sources,
                &mut frame,
            ))
            .unwrap();
        let timed = |label: &str, seconds: f64| {
            json!({
                "schema": SCHEMA, "case": "cost", "phase": label, "spacing_bytes": spacing,
                "blob_bytes": BLOB_BYTES, "frame_bytes": stats.frame_bytes,
                "copy_bytes": stats.copy_bytes, "literal_bytes": stats.literal_bytes,
                "wall_seconds": seconds, "correctness": "passed",
            })
        };
        let started = Instant::now();
        let mut output = Vec::new();
        rt.block_on(decode_sliced(
            &mut std::io::Cursor::new(&frame),
            new_id,
            new.len() as u64,
            &sources,
            &mut output,
        ))
        .unwrap();
        assert_eq!(output, new);
        let row = timed("decode_to_memory", started.elapsed().as_secs_f64());
        eprintln!("{row}");
        report.push(row);

        let frame_len = frame.len();
        group.bench_function(BenchmarkId::new("server_encode", human(spacing)), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let (new, index, sources) = (&new, &index, &sources);
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let started = Instant::now();
                        let stats = encode_sliced_stream(
                            new_id,
                            new.len() as u64,
                            std::io::Cursor::new(new),
                            index,
                            sources,
                            &mut tokio::io::sink(),
                        )
                        .await
                        .unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(stats.frame_bytes as usize, frame_len);
                    }
                    elapsed
                }
            })
        });
        group.bench_function(
            BenchmarkId::new("receiver_decode_sink", human(spacing)),
            |b| {
                b.to_async(&rt).iter_custom(|iterations| {
                    let (new, frame, sources) = (&new, &frame, &sources);
                    async move {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            let started = Instant::now();
                            let stats = decode_sliced(
                                &mut std::io::Cursor::new(frame),
                                new_id,
                                new.len() as u64,
                                sources,
                                &mut tokio::io::sink(),
                            )
                            .await
                            .unwrap();
                            elapsed += started.elapsed();
                            assert_eq!(stats.copy_bytes + stats.literal_bytes, new.len() as u64);
                        }
                        elapsed
                    }
                })
            },
        );
        group.bench_function(
            BenchmarkId::new("receiver_decode_store", human(spacing)),
            |b| {
                b.to_async(&rt).iter_custom(|iterations| {
                    let (new, frame, sources, store) = (&new, &frame, &sources, &store);
                    async move {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            let started = Instant::now();
                            let mut writer = store.open_write().await;
                            decode_sliced(
                                &mut std::io::Cursor::new(frame),
                                new_id,
                                new.len() as u64,
                                sources,
                                &mut *writer,
                            )
                            .await
                            .unwrap();
                            let (id, _) = writer.close().await.unwrap();
                            elapsed += started.elapsed();
                            assert_eq!(id, new_id);
                        }
                        elapsed
                    }
                })
            },
        );
    }
    group.finish();
}

/// A writer that takes one second per `bytes_per_second` bytes, so a frame
/// leaves at a fixed rate the way a real link drains one.
struct PacedWriter {
    bytes_per_second: u64,
    written: u64,
    /// The timer for the write in progress; one per write, not per poll.
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl PacedWriter {
    fn new(bytes_per_second: u64) -> Self {
        Self {
            bytes_per_second,
            written: 0,
            delay: None,
        }
    }
}

impl AsyncWrite for PacedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let rate = this.bytes_per_second;
        let sleep = this.delay.get_or_insert_with(|| {
            Box::pin(tokio::time::sleep(Duration::from_secs_f64(
                buffer.len() as f64 / rate as f64,
            )))
        });
        match sleep.as_mut().poll(context) {
            Poll::Ready(()) => {
                this.delay = None;
                this.written += buffer.len() as u64;
                Poll::Ready(Ok(buffer.len()))
            }
            // The timer wakes this task; the encoder runs meanwhile.
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Encoding a blob with no copy sources is literal segments plus compression,
/// so it is the case where encode time is worth overlapping with the link.
/// The shipped encoder streams window by window; the reference here encodes
/// the whole frame first and then writes it, as the encoder used to. The two
/// cross where link throughput passes the encoder's own.
fn pipeline(c: &mut Criterion, report: &mut Vec<Value>) {
    const PIPELINE_BYTES: usize = 32 * MIB;
    // The paced writer needs timers, which the shared bench runtime omits.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let bytes = bench_util::random_bytes(34, PIPELINE_BYTES);
    let id = id_of(&bytes);
    let index = SliceIndex::new(0);
    let sources = BlobSliceSources::new(MemoryBlobStore::new());
    let (frame, stats) = rt
        .block_on(encode_sliced(id, &bytes, &index, &sources))
        .unwrap();
    assert_eq!(stats.copies, 0);
    let row = json!({
        "schema": SCHEMA, "case": "pipeline", "blob_bytes": PIPELINE_BYTES,
        "frame_bytes": stats.frame_bytes, "literals": stats.literals,
        "correctness": "passed",
    });
    eprintln!("{row}");
    report.push(row);

    let mut group = c.benchmark_group("sliced_pipeline");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(PIPELINE_BYTES as u64));
    // Per-write delays stay above the timer's millisecond granularity: one
    // 1 MiB literal segment takes 8 ms at 128 MiB/s and 1 ms at 1 GiB/s.
    for rate in [128, 256, 512, 1024] {
        let bytes_per_second = rate * MIB as u64;
        group.bench_function(BenchmarkId::new("overlapped", rate), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let (bytes, index, sources) = (&bytes, &index, &sources);
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut paced = PacedWriter::new(bytes_per_second);
                        let started = Instant::now();
                        encode_sliced_stream(
                            id,
                            bytes.len() as u64,
                            std::io::Cursor::new(bytes),
                            index,
                            sources,
                            &mut paced,
                        )
                        .await
                        .unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(paced.written, stats.frame_bytes);
                    }
                    elapsed
                }
            })
        });
        group.bench_function(BenchmarkId::new("sequential", rate), |b| {
            b.to_async(&rt).iter_custom(|iterations| {
                let (bytes, index, sources, frame) = (&bytes, &index, &sources, &frame);
                async move {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut paced = PacedWriter::new(bytes_per_second);
                        let started = Instant::now();
                        let (encoded, _) = encode_sliced(id, bytes, index, sources).await.unwrap();
                        paced.write_all(&encoded).await.unwrap();
                        elapsed += started.elapsed();
                        assert_eq!(paced.written, frame.len() as u64);
                    }
                    elapsed
                }
            })
        });
    }
    group.finish();
}

fn sliced_transfer(c: &mut Criterion) {
    let mut report = Vec::new();
    codec(c, &mut report);
    costs(c, &mut report);
    pipeline(c, &mut report);
    transport(c, &mut report);
    if let Some(path) = std::env::var_os("CASITA_SLICED_REPORT") {
        // Every row above passed its reconstruction or closure gate.
        let document = json!({"schema": SCHEMA, "samples": report});
        std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }
}

criterion_group!(benches, sliced_transfer);
criterion_main!(benches);
