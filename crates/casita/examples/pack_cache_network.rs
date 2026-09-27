//! Real-S3 cache-pressure probe driven by `benchmark run pack-cache-network`.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use casita::experimental::object_store::aws::AmazonS3Builder;
use casita::experimental::object_store::{ObjectStore, path::Path};
use casita::experimental::{BlobId, BlobStore, ChunkedBlobStore, DEFAULT_AVG_CHUNK_SIZE};
use futures::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

const OBJECT_BYTES: usize = 64 * 1024;
const PACK_BYTES: u64 = 256 * 1024;
type Error = Box<dyn std::error::Error + Send + Sync>;

fn number(config: &Value, name: &str) -> usize {
    usize::try_from(config[name].as_u64().expect(name)).expect(name)
}

fn backend(endpoint: &str, bucket: &str) -> Result<Arc<dyn ObjectStore>, Error> {
    Ok(Arc::new(
        AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_access_key_id("minio")
            .with_secret_access_key("minio123")
            .with_region("us-east-1")
            .with_endpoint(endpoint)
            .with_allow_http(true)
            .build()?,
    ))
}

async fn open(objects: Arc<dyn ObjectStore>, config: &Value) -> Result<ChunkedBlobStore, Error> {
    Ok(ChunkedBlobStore::packed_with_options(
        objects,
        Path::from(config["prefix"].as_str().unwrap()),
        DEFAULT_AVG_CHUNK_SIZE,
        casita::experimental::PackOptions {
            target_size: PACK_BYTES,
            cache_capacity: number(config, "cache_bytes") as u64,
        },
    )
    .await?)
}

fn sequence(objects: usize, operations: usize, pattern: &str) -> Vec<usize> {
    let mut state = 0xCA517A_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize
    };
    let mut shuffled: Vec<_> = (0..objects).collect();
    for i in (1..objects).rev() {
        shuffled.swap(i, next() % (i + 1));
    }
    (0..operations)
        .map(|i| match pattern {
            "sequential" => i % objects,
            "random" => shuffled[i % objects],
            "skewed" if i % 5 == 0 => next() % objects,
            "skewed" => next() % (objects / 8).max(1),
            _ => panic!("unknown pattern"),
        })
        .collect()
}

async fn read(store: &ChunkedBlobStore, id: &BlobId) -> Result<Vec<u8>, Error> {
    let mut reader = store.open_read(id).await?.ok_or("missing payload")?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let config: Value =
        serde_json::from_str(&std::env::args().nth(1).ok_or("missing JSON config")?)?;
    let working = number(&config, "working_set_bytes");
    let cache = number(&config, "cache_bytes");
    let operations = number(&config, "reads");
    let concurrency = number(&config, "concurrency");
    assert!(working >= 2 * OBJECT_BYTES && working.is_multiple_of(OBJECT_BYTES));
    assert!(
        cache >= PACK_BYTES as usize && concurrency > 0 && operations >= working / OBJECT_BYTES
    );
    let pattern = config["pattern"].as_str().unwrap();
    let expected: Vec<Vec<u8>> = (0..working / OBJECT_BYTES)
        .map(|index| {
            let mut bytes = vec![0; OBJECT_BYTES];
            blake3::Hasher::new()
                .update(&(index as u64).to_le_bytes())
                .finalize_xof()
                .fill(&mut bytes);
            bytes
        })
        .collect();
    let ids: Vec<_> = expected
        .iter()
        .map(|bytes| BlobId::new(blake3::hash(bytes).into()))
        .collect();
    let mut fixture_hash = blake3::Hasher::new();
    for bytes in &expected {
        fixture_hash.update(bytes);
    }
    let order = sequence(ids.len(), operations, pattern);
    let mut order_hash = blake3::Hasher::new();
    for &index in &order {
        order_hash.update(&(index as u64).to_le_bytes());
    }

    // Setup uses the unshaped endpoint; timed reads use only the proxy endpoint.
    let objects = backend(
        config["backend_endpoint"].as_str().unwrap(),
        config["bucket"].as_str().unwrap(),
    )?;
    let store = open(objects.clone(), &config).await?;
    let batch = store.begin_batch();
    for bytes in &expected {
        store.put_slice(bytes).await?;
    }
    store.flush().await?;
    drop(batch);
    drop(store);
    let packs = objects
        .list(Some(&Path::from(format!(
            "{}/packs",
            config["prefix"].as_str().unwrap()
        ))))
        .try_collect::<Vec<_>>()
        .await?;
    let physical_bytes: u64 = packs.iter().map(|pack| pack.size).sum();
    assert!(!packs.is_empty());
    assert_eq!(physical_bytes < cache as u64, working < cache);

    for phase in config["phases"].as_array().unwrap() {
        let phase = phase.as_str().unwrap();
        assert!(matches!(phase, "cold" | "warm"));
        let store = open(
            backend(
                config["read_endpoint"].as_str().unwrap(),
                config["bucket"].as_str().unwrap(),
            )?,
            &config,
        )
        .await?;
        if phase == "warm" {
            for (id, bytes) in ids.iter().zip(&expected) {
                assert_eq!(&read(&store, id).await?, bytes);
            }
        }
        store.reset_pack_read_stats();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let started = Instant::now();
        let mut nanos: Vec<u64> = futures::stream::iter(order.iter().copied())
            .map(|index| {
                let (store, ids, expected, active, peak) =
                    (&store, &ids, &expected, &active, &peak);
                async move {
                    let now = active.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(now, Ordering::Relaxed);
                    let started = Instant::now();
                    let actual = read(store, &ids[index]).await?;
                    let elapsed = started.elapsed().as_nanos() as u64;
                    active.fetch_sub(1, Ordering::Relaxed);
                    assert_eq!(actual, expected[index]);
                    Ok::<_, Error>(elapsed)
                }
            })
            .buffer_unordered(concurrency)
            .try_collect()
            .await?;
        let wall_seconds = started.elapsed().as_secs_f64();
        let stats = store.pack_read_stats().unwrap();
        if working < cache && phase == "warm" {
            assert_eq!(stats.chunk_range_requests + stats.whole_pack_requests, 0);
        }
        if working > cache && pattern == "sequential" {
            assert!(stats.cache_evictions > 0);
        }
        nanos.sort_unstable();
        let percentile = |p: usize| nanos[(nanos.len() * p).div_ceil(100).saturating_sub(1)];
        println!(
            "cache_network_sample {}",
            json!({
                "status": "ok", "implementation": "casita", "operation": format!("{pattern}-{phase}"),
                "operations": operations, "wall_seconds": wall_seconds,
                "p50_nanos": percentile(50), "p95_nanos": percentile(95), "p99_nanos": percentile(99),
                "max_nanos": nanos.last().unwrap(), "concurrency": concurrency,
                "max_in_flight_reads": peak.load(Ordering::Relaxed),
                "cache_bytes": cache, "working_set_bytes": working, "physical_pack_bytes": physical_bytes,
                "pack_count": packs.len(), "pack_target_bytes": PACK_BYTES, "file_bytes": OBJECT_BYTES,
                "logical_read_bytes": operations * OBJECT_BYTES,
                "fixture_blake3": fixture_hash.finalize().to_hex().to_string(),
                "access_order_blake3": order_hash.finalize().to_hex().to_string(),
                "pack_range_requests": stats.chunk_range_requests, "whole_pack_requests": stats.whole_pack_requests,
                "backend_read_bytes": stats.chunk_range_bytes + stats.whole_pack_bytes,
                "cache_hits": stats.cache_hits, "cache_evictions": stats.cache_evictions,
                "cache_promotions": stats.cache_promotions,
                "correctness": "every read compared byte-for-byte; wall includes verification, per-read latency excludes it"
            })
        );
    }
    Ok(())
}
