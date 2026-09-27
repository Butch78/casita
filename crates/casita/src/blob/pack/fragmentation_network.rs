//! Real S3 reconstruction probe, driven by `benchmark run s3-fragmentation`.
use super::*;
use crate::blob::{BlobStore, ChunkedBlobStore, DEFAULT_AVG_CHUNK_SIZE};
use async_trait::async_trait;
use object_store::{aws::AmazonS3Builder, *};
use serde_json::{Value, json};
use std::num::NonZeroUsize;
use tokio::io::AsyncReadExt;

#[derive(Debug, Default)]
struct Counts {
    gets: AtomicU64,
    read_bytes: AtomicU64,
    heads: AtomicU64,
    puts: AtomicU64,
    write_bytes: AtomicU64,
    lists: AtomicU64,
    pack_io: StdMutex<BTreeMap<String, (u64, u64, u64)>>,
}
impl Counts {
    fn reset(&self) {
        self.pack_io.lock().unwrap().clear();
        for counter in [
            &self.gets,
            &self.read_bytes,
            &self.heads,
            &self.puts,
            &self.write_bytes,
            &self.lists,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
    fn json(&self) -> Value {
        json!({"gets": self.gets.load(Ordering::Relaxed), "get_bytes": self.read_bytes.load(Ordering::Relaxed),
            "heads": self.heads.load(Ordering::Relaxed), "puts": self.puts.load(Ordering::Relaxed),
            "put_bytes": self.write_bytes.load(Ordering::Relaxed), "lists": self.lists.load(Ordering::Relaxed)})
    }
    fn pack_io(&self) -> Value {
        Value::Array(self.pack_io.lock().unwrap().iter().map(|(path, (gets, bytes, whole))|
            json!({"path": path, "gets": gets, "bytes": bytes, "whole_gets": whole})).collect())
    }
}

#[derive(Debug)]
struct Counted {
    inner: Arc<dyn ObjectStore>,
    counts: Arc<Counts>,
}
impl std::fmt::Display for Counted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Counted({})", self.inner)
    }
}
#[async_trait]
impl ObjectStore for Counted {
    async fn put_opts(
        &self,
        path: &Path,
        data: PutPayload,
        options: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.counts.puts.fetch_add(1, Ordering::Relaxed);
        self.counts
            .write_bytes
            .fetch_add(data.content_length() as u64, Ordering::Relaxed);
        self.inner.put_opts(path, data, options).await
    }
    async fn put_multipart_opts(
        &self,
        _: &Path,
        _: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        panic!("probe accounting requires the ordinary PUT path");
    }
    async fn get_opts(&self, path: &Path, options: GetOptions) -> object_store::Result<GetResult> {
        let head = options.head;
        let whole = options.range.is_none();
        let result = self.inner.get_opts(path, options).await?;
        if head {
            self.counts.heads.fetch_add(1, Ordering::Relaxed);
        } else {
            if path.as_ref().contains("/packs/") {
                let mut packs = self.counts.pack_io.lock().unwrap();
                let counts = packs.entry(path.to_string()).or_default();
                counts.0 += 1;
                counts.1 += result.range.end - result.range.start;
                counts.2 += u64::from(whole);
            }
            self.counts.gets.fetch_add(1, Ordering::Relaxed);
            self.counts
                .read_bytes
                .fetch_add(result.range.end - result.range.start, Ordering::Relaxed);
        }
        Ok(result)
    }
    fn delete_stream(
        &self,
        paths: futures::stream::BoxStream<'static, object_store::Result<Path>>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<Path>> {
        self.inner.delete_stream(paths)
    }
    fn list(
        &self,
        prefix: Option<&Path>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.counts.lists.fetch_add(1, Ordering::Relaxed);
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

fn number(config: &Value, key: &str) -> u64 {
    config[key].as_u64().expect(key)
}
fn text<'a>(config: &'a Value, key: &str) -> &'a str {
    config[key].as_str().expect(key)
}
fn bytes(label: u64, out: &mut [u8]) {
    blake3::Hasher::new()
        .update(b"casita-s3-fragmentation-v1")
        .update(&label.to_le_bytes())
        .finalize_xof()
        .fill(out);
}
fn corpus(config: &Value) -> Vec<u8> {
    if let Some(path) = config["artifact"].as_str() {
        std::fs::read(path).unwrap()
    } else {
        let mut out = vec![0; number(config, "file_bytes") as usize];
        bytes(0, &mut out);
        out
    }
}
fn edit(data: &mut [u8], generation: usize) {
    assert!(data.len() >= 1024 * 1024 && (1..=32).contains(&generation));
    // Permute 32 disjoint regions across the complete payload. This is a
    // controlled mutation of build-output bytes, not an actual rebuild trace.
    let offset = ((generation - 1) * 17 % 32) * (data.len() / 32) + 8192;
    bytes(generation as u64, &mut data[offset..offset + 4096]);
}
fn backend(config: &Value, endpoint: &str, counts: Arc<Counts>) -> Arc<dyn ObjectStore> {
    Arc::new(Counted {
        inner: Arc::new(
            AmazonS3Builder::new()
                .with_bucket_name(text(config, "bucket"))
                .with_access_key_id("minio")
                .with_secret_access_key("minio123")
                .with_region("us-east-1")
                .with_endpoint(endpoint)
                .with_allow_http(true)
                .build()
                .unwrap(),
        ),
        counts,
    })
}
async fn open(objects: Arc<dyn ObjectStore>, prefix: &str, cache: u64) -> ChunkedBlobStore {
    ChunkedBlobStore::packed_with_options(
        objects,
        Path::from(prefix),
        DEFAULT_AVG_CHUNK_SIZE,
        crate::PackOptions {
            target_size: DEFAULT_PACK_TARGET_SIZE,
            cache_capacity: cache,
        },
    )
    .await
    .unwrap()
    .with_chunk_upload_concurrency(NonZeroUsize::new(1).unwrap())
}
async fn read(store: &ChunkedBlobStore, expected: &[u8]) -> u64 {
    let id = BlobId::new(blake3::hash(expected).into());
    let started = Instant::now();
    let mut reader = store.open_read(&id).await.unwrap().unwrap();
    let mut out = Vec::with_capacity(expected.len());
    reader.read_to_end(&mut out).await.unwrap();
    let nanos = started.elapsed().as_nanos() as u64;
    assert_eq!(out, expected);
    assert_eq!(BlobId::new(blake3::hash(&out).into()), id);
    nanos
}

async fn measured_read(
    store: &ChunkedBlobStore,
    expected: &[u8],
    planned: Option<&Arc<super::planned_reads::PlannedReader>>,
    limit: usize,
    strategy: &str,
) -> (u64, u64) {
    let id = BlobId::new(blake3::hash(expected).into());
    let started = Instant::now();
    let wanted = if limit == 0 {
        expected.len()
    } else {
        limit.min(expected.len())
    };
    let partial = wanted < expected.len();
    let reader: Box<dyn tokio::io::AsyncRead + Send + Unpin> = if let Some(planned) = planned {
        let mut chunks = store.chunks(&id).await.unwrap().unwrap();
        if partial {
            let mut covered = 0;
            let keep = chunks
                .iter()
                .position(|chunk| {
                    covered += chunk.size;
                    covered >= wanted as u64
                })
                .unwrap()
                + 1;
            chunks.truncate(keep);
        }
        if strategy != "planned" {
            Box::new(
                planned
                    .reader_mode(chunks, (!partial).then_some(id), strategy == "lookahead")
                    .await
                    .unwrap(),
            )
        } else {
            Box::new(tokio_util::io::StreamReader::new(
                planned.stream(chunks, (!partial).then_some(id)),
            ))
        }
    } else {
        Box::new(store.open_read(&id).await.unwrap().unwrap())
    };
    let mut reader: Box<dyn tokio::io::AsyncRead + Send + Unpin> = if partial {
        Box::new(reader.take(wanted as u64))
    } else {
        reader
    };
    let mut actual = Vec::with_capacity(wanted);
    actual.push(0);
    reader.read_exact(&mut actual).await.unwrap();
    let first_byte_nanos = started.elapsed().as_nanos() as u64;
    reader.read_to_end(&mut actual).await.unwrap();
    let nanos = started.elapsed().as_nanos() as u64;
    assert_eq!(actual, expected[..wanted]);
    assert_eq!(blake3::hash(&actual), blake3::hash(&expected[..wanted]));
    drop(reader);
    // Settle aborted look-ahead before sampling/resetting shared counters.
    // Shutdown is outside the measured user-visible read latency.
    crate::metadata::flush_repository_leases().await.unwrap();
    (nanos, first_byte_nanos)
}

async fn describe(objects: Arc<dyn ObjectStore>, prefix: &str, data: &[u8]) -> Value {
    let store = open(objects.clone(), prefix, 0).await;
    let id = BlobId::new(blake3::hash(data).into());
    let chunks = store.chunks(&id).await.unwrap().unwrap();
    let catalog = PackedChunks::open(
        objects.clone(),
        Path::from(prefix),
        DEFAULT_PACK_TARGET_SIZE,
    )
    .await
    .unwrap();
    let mut packs = BTreeMap::new();
    let mut read_plan = Vec::new();
    let mut runs = 0;
    let mut previous = None;
    for chunk in &chunks {
        let location = catalog.location(&chunk.digest).await.unwrap().unwrap();
        read_plan.push(
            json!({"digest": chunk.digest.to_string(), "size": chunk.size,
            "pack": location.pack.to_string(), "pack_len": location.pack_len,
            "offset": location.offset, "framed_len": location.framed_len}),
        );
        if previous != Some(location.pack) {
            runs += 1;
        }
        previous = Some(location.pack);
        packs.insert(location.pack, location.pack_len);
    }
    let inventory = objects
        .list(Some(&Path::from(prefix)))
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    json!({"prefix": prefix, "blob": id.to_string(), "file_bytes": data.len(), "chunks": chunks.len(), "read_plan": read_plan,
        "referenced_packs": packs.len(), "pack_runs": runs, "largest_pack_bytes": packs.values().max().unwrap(),
        "referenced_pack_bytes": packs.values().sum::<u64>(), "stored_bytes": inventory.iter().map(|o| o.size).sum::<u64>(),
        "stored_pack_bytes": inventory.iter().filter(|o| o.location.as_ref().starts_with(&format!("{prefix}/packs/"))).map(|o| o.size).sum::<u64>()})
}

async fn prepare(config: &Value) {
    let counts = Arc::new(Counts::default());
    let objects = backend(config, text(config, "endpoint"), counts.clone());
    let mut data = corpus(config);
    assert!(data.len() >= 1024 * 1024);
    let original_hash = blake3::hash(&data).to_hex().to_string();
    let prefix = text(config, "prefix");
    let history_prefix = format!("{prefix}/history");
    let fresh_prefix = format!("{prefix}/fresh");
    let store = open(objects.clone(), &history_prefix, 0).await;
    counts.reset();
    let generations = number(config, "generations") as usize;
    assert!((1..=32).contains(&generations));
    let mut writes = Vec::new();
    for generation in 0..=generations {
        if generation > 0 {
            edit(&mut data, generation);
        }
        counts.reset();
        let started = Instant::now();
        let id = store.put_slice(&data).await.unwrap();
        store.flush().await.unwrap();
        let nanos = started.elapsed().as_nanos() as u64;
        assert_eq!(id, BlobId::new(blake3::hash(&data).into()));
        writes.push(json!({"generation": generation, "nanos": nanos, "origin": counts.json()}));
    }
    drop(store);
    // Audit the complete retained history without retaining N copies in RAM.
    let verifier = open(objects.clone(), &history_prefix, 0).await;
    let mut expected = corpus(config);
    for generation in 0..=generations {
        if generation > 0 {
            edit(&mut expected, generation);
        }
        read(&verifier, &expected).await;
    }
    assert_eq!(data, expected);
    drop(expected);
    drop(verifier);
    let fresh = open(objects.clone(), &fresh_prefix, 0).await;
    fresh.put_slice(&data).await.unwrap();
    fresh.flush().await.unwrap();
    read(&fresh, &data).await;
    drop(fresh);
    let history = describe(objects.clone(), &history_prefix, &data).await;
    let fresh = describe(objects, &fresh_prefix, &data).await;
    assert_eq!(history["blob"], fresh["blob"]);
    println!(
        "s3_fragmentation_prepared {}",
        json!({"history": history, "fresh": fresh, "writes": writes,
        "original_blake3": original_hash, "generations": generations, "pack_target_bytes": DEFAULT_PACK_TARGET_SIZE,
        "default_cache_bytes": DEFAULT_PACK_CACHE_CAPACITY, "avg_chunk_bytes": DEFAULT_AVG_CHUNK_SIZE,
        "correctness": "all historical versions and fresh control reconstructed and independently hashed"})
    );
}

async fn measure(config: &Value) {
    let mut data = corpus(config);
    for generation in 1..=number(config, "generations") as usize {
        edit(&mut data, generation);
    }
    let prepared = &config["prepared"];
    let layout = text(config, "layout");
    let descriptor = &prepared[layout];
    assert_eq!(
        descriptor["blob"],
        BlobId::new(blake3::hash(&data).into()).to_string()
    );
    let counts = Arc::new(Counts::default());
    let objects = backend(config, text(config, "endpoint"), counts.clone());
    let cache = number(config, "cache_bytes");
    let start = Instant::now();
    let store = open(objects, text(descriptor, "prefix"), cache).await;
    let strategy = config["read_strategy"].as_str().unwrap_or("current");
    let planned = (strategy != "current").then(|| {
        super::planned_reads::PlannedReader::new(
            store.benchmark_packed(),
            cache,
            config["window_bytes"].as_u64().unwrap_or(16 * 1024 * 1024),
        )
    });
    let limit = config["read_bytes"].as_u64().unwrap_or(0) as usize;
    let reopen_nanos = start.elapsed().as_nanos() as u64;
    let reopen_origin = counts.json();
    for phase in ["cold", "warm"] {
        if phase == "warm" {
            measured_read(&store, &data, planned.as_ref(), limit, strategy).await;
        }
        counts.reset();
        store.reset_pack_read_stats();
        if let Some(planned) = &planned {
            planned.meter.reset();
        }
        let (nanos, first_byte_nanos) =
            measured_read(&store, &data, planned.as_ref(), limit, strategy).await;
        let stats = store.pack_read_stats().unwrap();
        let requests =
            stats.chunk_range_requests + stats.whole_pack_requests + stats.footer_range_requests;
        let read_bytes =
            stats.chunk_range_bytes + stats.whole_pack_bytes + stats.footer_range_bytes;
        if phase == "cold" {
            assert!(requests > 0 && read_bytes > 0);
        }
        if cache == 0 {
            assert_eq!(stats.whole_pack_requests, 0);
        }
        if phase == "warm" && limit == 0 && cache >= number(descriptor, "referenced_pack_bytes") {
            assert_eq!(requests, 0);
        }
        if limit == 0 {
            assert!(counts.gets.load(Ordering::Relaxed) >= requests);
        }
        assert!(counts.read_bytes.load(Ordering::Relaxed) >= read_bytes);
        assert_eq!(counts.puts.load(Ordering::Relaxed), 0);
        println!(
            "s3_fragmentation_sample {}",
            json!({"phase": phase, "layout": layout, "cache": config["cache"],
            "cache_bytes": cache, "blob": descriptor["blob"], "file_bytes": data.len(), "nanos": nanos,
            "first_byte_nanos": first_byte_nanos, "read_strategy": strategy,
            "production_fetch": if strategy == "current" { Some("planned-chunk-cache-v1") } else { None }, "read_bytes": limit,
            "response_peak_bytes": planned.as_ref().map(|p| p.meter.peak()),
            "pack_requests": requests, "pack_read_bytes": read_bytes, "whole_pack_requests": stats.whole_pack_requests,
            "chunk_range_requests": stats.chunk_range_requests, "cache_hits": stats.cache_hits,
            "cache_promotions": stats.cache_promotions, "cache_evictions": stats.cache_evictions,
            "origin": counts.json(), "pack_io": counts.pack_io(), "reopen_nanos": reopen_nanos, "reopen_origin": reopen_origin,
            "correctness": "exact bytes and independent BLAKE3; ample warm cache has zero pack GETs"})
        );
    }
}

#[tokio::test]
#[ignore = "real S3 probe; benchmark run s3-fragmentation"]
async fn benchmark_s3_fragmentation() {
    let config: Value =
        serde_json::from_str(&std::env::var("CASITA_S3_FRAGMENTATION").unwrap()).unwrap();
    match text(&config, "mode") {
        "prepare" => prepare(&config).await,
        "read" => measure(&config).await,
        _ => panic!("unknown mode"),
    }
}
