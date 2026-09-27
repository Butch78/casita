//! Benchmark helper for `benchmark run s3-pack-index`.
//!
//! It creates a fresh RustFS bucket, imports one tree through the real S3/wal3
//! profile, deletes the advisory pointer, then proves cold and warm reopens
//! recover the catalog directly from WAL3 state.

#[cfg(not(feature = "s3"))]
fn main() {
    eprintln!("pack_index_rustfs requires --features s3");
    std::process::exit(2);
}

#[cfg(feature = "s3")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::sync::Arc;
    use std::time::Instant;

    use casita::experimental::object_store::aws::AmazonS3Builder;
    use casita::experimental::object_store::path::Path;
    use casita::experimental::object_store::{ObjectStore, ObjectStoreExt};
    use casita::experimental::{
        ChunkedBlobStore, MetadataMutation, MetadataStore, Repository, RootName, Wal3MetadataStore,
        Wal3ReadStats,
    };
    use chroma_config::Configurable;
    use futures::TryStreamExt;

    type S3Repository = Repository<ChunkedBlobStore, Wal3MetadataStore>;

    fn print_wal_stats(prefix: &str, stats: Wal3ReadStats) {
        println!(
            "{prefix}-wal-writer-open-requests {}",
            stats.writer_open_requests
        );
        println!("{prefix}-wal-writer-open-nanos {}", stats.writer_open_nanos);
        println!(
            "{prefix}-wal-manifest-load-requests {}",
            stats.manifest_load_requests
        );
        println!(
            "{prefix}-wal-manifest-load-nanos {}",
            stats.manifest_load_nanos
        );
        println!(
            "{prefix}-wal-manifest-refresh-requests {}",
            stats.manifest_refresh_requests
        );
        println!(
            "{prefix}-wal-manifest-refresh-nanos {}",
            stats.manifest_refresh_nanos
        );
        println!(
            "{prefix}-wal-fragment-get-requests {}",
            stats.fragment_get_requests
        );
        println!(
            "{prefix}-wal-fragment-get-bytes {}",
            stats.fragment_get_bytes
        );
        println!("{prefix}-wal-fragment-records {}", stats.fragment_records);
        println!(
            "{prefix}-wal-fragment-record-bytes {}",
            stats.fragment_record_bytes
        );
        println!(
            "{prefix}-wal-fragment-get-nanos {}",
            stats.fragment_get_nanos
        );
        println!(
            "{prefix}-wal-parquet-parse-nanos {}",
            stats.parquet_parse_nanos
        );
        println!(
            "{prefix}-wal-state-decode-nanos {}",
            stats.state_decode_nanos
        );
        println!(
            "{prefix}-wal-fragment-put-requests {}",
            stats.fragment_put_requests
        );
        println!(
            "{prefix}-wal-manifest-put-requests {}",
            stats.manifest_put_requests
        );
        println!("{prefix}-wal-cache-hits {}", stats.checkpoint_cache_hits);
        println!(
            "{prefix}-wal-cache-misses {}",
            stats.checkpoint_cache_misses
        );
        println!("{prefix}-wal-checkpoint-bytes {}", stats.checkpoint_bytes);
        println!(
            "{prefix}-wal-checkpoint-objects {}",
            stats.checkpoint_objects
        );
        println!("{prefix}-wal-checkpoint-roots {}", stats.checkpoint_roots);
        println!(
            "{prefix}-wal-checkpoint-validated {}",
            stats.checkpoint_validated
        );
        println!("{prefix}-wal-tail-deltas {}", stats.tail_deltas);
        println!(
            "{prefix}-wal-logical-shard-get-requests {}",
            stats.logical_shard_get_requests
        );
        println!(
            "{prefix}-wal-logical-shard-put-requests {}",
            stats.logical_shard_put_requests
        );
        println!(
            "{prefix}-wal-logical-shard-barrier-get-requests {}",
            stats.logical_shard_barrier_get_requests
        );
        println!(
            "{prefix}-wal-logical-shard-barrier-put-requests {}",
            stats.logical_shard_barrier_put_requests
        );
        println!(
            "{prefix}-wal-logical-shard-inventory-list-requests {}",
            stats.logical_shard_inventory_list_requests
        );
        println!(
            "{prefix}-wal-logical-shard-delete-requests {}",
            stats.logical_shard_delete_requests
        );
    }

    async fn open_repository(
        storage: Arc<chroma_storage::Storage>,
        bucket: &str,
        endpoint: &str,
        prefix: &str,
        writer: &str,
        target: u64,
    ) -> Result<(S3Repository, u64, u64), Box<dyn std::error::Error + Send + Sync>> {
        let objects = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_access_key_id("minio")
            .with_secret_access_key("minio123")
            .with_region("us-east-1")
            .with_endpoint(endpoint)
            .with_allow_http(true)
            .with_checksum_algorithm(casita::experimental::object_store::aws::Checksum::SHA256)
            .build()?;
        let started = Instant::now();
        let state = Wal3MetadataStore::open(storage, format!("{prefix}/state"), writer)
            .await
            .map_err(std::io::Error::other)?;
        let state_elapsed = started.elapsed();
        let snapshot = state.opened_snapshot();
        let catalog = snapshot
            .payload_catalog()
            .ok_or("WAL3 state does not contain a payload catalog")?;
        let started = Instant::now();
        let payloads = ChunkedBlobStore::packed_with_catalog(
            Arc::new(objects),
            Path::from(format!("{prefix}/payloads")),
            casita::experimental::DEFAULT_AVG_CHUNK_SIZE,
            casita::experimental::PackOptions {
                target_size: target,
                cache_capacity: 0,
            },
            catalog,
        )
        .await
        .map_err(std::io::Error::other)?;
        let payload_elapsed = started.elapsed();
        drop(snapshot);
        Ok((
            Repository::new(payloads, state),
            u64::try_from(payload_elapsed.as_nanos()).unwrap_or(u64::MAX),
            u64::try_from(state_elapsed.as_nanos()).unwrap_or(u64::MAX),
        ))
    }

    let mut args = std::env::args_os().skip(1);
    let source = args.next().ok_or("missing source tree")?;
    let target: u64 = args
        .next()
        .ok_or("missing pack target")?
        .to_str()
        .ok_or("pack target is not UTF-8")?
        .parse()?;
    let writer = args
        .next()
        .ok_or("missing writer")?
        .to_str()
        .ok_or("writer is not UTF-8")?
        .to_owned();
    let endpoint = args
        .next()
        .ok_or("missing RustFS endpoint")?
        .to_str()
        .ok_or("RustFS endpoint is not UTF-8")?
        .to_owned();
    if args.next().is_some() {
        return Err("unexpected trailing argument".into());
    }

    let bucket = format!("test-{writer}");
    let credentials = aws_sdk_s3::config::Credentials::new(
        "minio",
        "minio123",
        None,
        None,
        "casita-rustfs-benchmark",
    );
    let client_config = aws_sdk_s3::config::Builder::new()
        .endpoint_url(&endpoint)
        .credentials_provider(credentials)
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(client_config)
        .create_bucket()
        .bucket(&bucket)
        .send()
        .await?;
    let storage_config =
        chroma_storage::config::StorageConfig::S3(chroma_storage::S3StorageConfig {
            bucket: bucket.clone(),
            credentials: chroma_storage::S3CredentialsConfig::Explicit {
                access_key_id: "minio".to_owned(),
                secret_access_key: "minio123".to_owned(),
                session_token: None,
                custom_endpoint: Some(endpoint.clone()),
                region: "us-east-1".to_owned(),
            },
            ..Default::default()
        });
    let storage = Arc::new(chroma_storage::Storage::S3(
        chroma_storage::S3Storage::try_from_config(
            &storage_config,
            &chroma_config::registry::Registry::default(),
        )
        .await
        .map_err(|error| error.to_string())?,
    ));
    let prefix = format!("pack-index/{writer}");
    let name = RootName::try_from("bench/current")?;

    let (repository, _, _) = open_repository(
        storage.clone(),
        &bucket,
        &endpoint,
        &prefix,
        &format!("{writer}-seed"),
        target,
    )
    .await?;
    let root = repository
        .import(casita::import::FilesystemImport::new(source, name))
        .await?;
    // Put the measured reopen at exactly one delta after a full checkpoint,
    // regardless of how many bounded import commits the corpus required.
    let mut revision = repository.metadata().snapshot().await?.revision();
    while repository.metadata().read_stats().tail_deltas != 0 {
        revision = repository
            .metadata()
            .commit(&revision, MetadataMutation::new())
            .await?
            .revision;
    }
    repository
        .metadata()
        .commit(&revision, MetadataMutation::new())
        .await?;
    drop(repository);

    let maintenance = AmazonS3Builder::new()
        .with_bucket_name(&bucket)
        .with_access_key_id("minio")
        .with_secret_access_key("minio123")
        .with_region("us-east-1")
        .with_endpoint(&endpoint)
        .with_allow_http(true)
        .with_checksum_algorithm(casita::experimental::object_store::aws::Checksum::SHA256)
        .build()?;
    let pack_prefix = Path::from(format!("{prefix}/payloads/packs"));
    let pack_count = maintenance
        .list(Some(&pack_prefix))
        .try_collect::<Vec<_>>()
        .await?
        .len();
    if pack_count == 0 {
        return Err("import produced no packs".into());
    }
    maintenance
        .delete(&Path::from(format!("{prefix}/payloads/pack-index-current")))
        .await?;

    let started = Instant::now();
    let (cold, cold_payload_nanos, cold_state_nanos) = open_repository(
        storage.clone(),
        &bucket,
        &endpoint,
        &prefix,
        &format!("{writer}-cold"),
        target,
    )
    .await?;
    let cold_elapsed = started.elapsed();
    let cold_open_wal_stats = cold.metadata().read_stats();
    cold.metadata().reset_read_stats();
    let snapshot_started = Instant::now();
    let _ = cold.metadata().snapshot().await?;
    let cold_first_snapshot_elapsed = snapshot_started.elapsed();
    let cold_command_elapsed = started.elapsed();
    let cold_first_snapshot_wal_stats = cold.metadata().read_stats();
    cold.metadata().reset_read_stats();
    let snapshot_started = Instant::now();
    let _ = cold.metadata().snapshot().await?;
    let cold_repeat_snapshot_elapsed = snapshot_started.elapsed();
    let cold_repeat_snapshot_wal_stats = cold.metadata().read_stats();
    let cold_stats = cold
        .payloads()
        .pack_read_stats()
        .ok_or("cold S3 store did not expose pack statistics")?;
    if !matches!(
        cold.verify_closure(&root).await?,
        casita::experimental::ClosureStatus::Complete { .. }
    ) {
        return Err("cold-opened S3 closure is incomplete".into());
    }
    drop(cold);

    let started = Instant::now();
    let (warm, warm_payload_nanos, warm_state_nanos) = open_repository(
        storage,
        &bucket,
        &endpoint,
        &prefix,
        &format!("{writer}-warm"),
        target,
    )
    .await?;
    let warm_elapsed = started.elapsed();
    let warm_open_wal_stats = warm.metadata().read_stats();
    warm.metadata().reset_read_stats();
    let snapshot_started = Instant::now();
    let _ = warm.metadata().snapshot().await?;
    let warm_first_snapshot_elapsed = snapshot_started.elapsed();
    let warm_command_elapsed = started.elapsed();
    let warm_first_snapshot_wal_stats = warm.metadata().read_stats();
    warm.metadata().reset_read_stats();
    let snapshot_started = Instant::now();
    let _ = warm.metadata().snapshot().await?;
    let warm_repeat_snapshot_elapsed = snapshot_started.elapsed();
    let warm_repeat_snapshot_wal_stats = warm.metadata().read_stats();
    let warm_stats = warm
        .payloads()
        .pack_read_stats()
        .ok_or("warm S3 store did not expose pack statistics")?;
    if !matches!(
        warm.verify_closure(&root).await?,
        casita::experimental::ClosureStatus::Complete { .. }
    ) {
        return Err("warm-opened S3 closure is incomplete".into());
    }

    println!("pack-count {pack_count}");
    println!("cold-wall-nanos {}", cold_elapsed.as_nanos());
    println!("cold-command-nanos {}", cold_command_elapsed.as_nanos());
    println!(
        "cold-first-snapshot-nanos {}",
        cold_first_snapshot_elapsed.as_nanos()
    );
    println!(
        "cold-repeat-snapshot-nanos {}",
        cold_repeat_snapshot_elapsed.as_nanos()
    );
    println!("cold-payload-open-nanos {cold_payload_nanos}");
    println!("cold-state-open-nanos {cold_state_nanos}");
    println!("cold-list-requests {}", cold_stats.list_requests);
    println!(
        "cold-footer-range-requests {}",
        cold_stats.footer_range_requests
    );
    println!("cold-footer-range-bytes {}", cold_stats.footer_range_bytes);
    println!(
        "cold-index-pointer-requests {}",
        cold_stats.index_pointer_requests
    );
    println!("cold-index-requests {}", cold_stats.index_requests);
    println!("cold-index-bytes {}", cold_stats.index_bytes);
    println!("cold-index-hash-nanos {}", cold_stats.index_hash_nanos);
    println!("cold-index-decode-nanos {}", cold_stats.index_decode_nanos);
    println!("cold-index-hits {}", cold_stats.index_hits);
    println!("cold-index-fallbacks {}", cold_stats.index_fallbacks);
    println!("cold-index-put-requests {}", cold_stats.index_put_requests);
    println!("cold-index-put-bytes {}", cold_stats.index_put_bytes);
    println!(
        "cold-index-sharded-base {}",
        u8::from(cold_stats.index_sharded_base)
    );
    println!(
        "cold-index-checkpoint-base {}",
        u8::from(cold_stats.index_checkpoint_base)
    );
    println!("cold-index-run-objects {}", cold_stats.index_run_objects);
    println!("warm-wall-nanos {}", warm_elapsed.as_nanos());
    println!("warm-command-nanos {}", warm_command_elapsed.as_nanos());
    println!(
        "warm-first-snapshot-nanos {}",
        warm_first_snapshot_elapsed.as_nanos()
    );
    println!(
        "warm-repeat-snapshot-nanos {}",
        warm_repeat_snapshot_elapsed.as_nanos()
    );
    println!("warm-payload-open-nanos {warm_payload_nanos}");
    println!("warm-state-open-nanos {warm_state_nanos}");
    println!("warm-list-requests {}", warm_stats.list_requests);
    println!(
        "warm-footer-range-requests {}",
        warm_stats.footer_range_requests
    );
    println!("warm-footer-range-bytes {}", warm_stats.footer_range_bytes);
    println!(
        "warm-index-pointer-requests {}",
        warm_stats.index_pointer_requests
    );
    println!("warm-index-requests {}", warm_stats.index_requests);
    println!("warm-index-bytes {}", warm_stats.index_bytes);
    println!("warm-index-hash-nanos {}", warm_stats.index_hash_nanos);
    println!("warm-index-decode-nanos {}", warm_stats.index_decode_nanos);
    println!("warm-index-hits {}", warm_stats.index_hits);
    println!("warm-index-fallbacks {}", warm_stats.index_fallbacks);
    println!("warm-index-put-requests {}", warm_stats.index_put_requests);
    println!("warm-index-put-bytes {}", warm_stats.index_put_bytes);
    println!(
        "warm-index-sharded-base {}",
        u8::from(warm_stats.index_sharded_base)
    );
    println!(
        "warm-index-checkpoint-base {}",
        u8::from(warm_stats.index_checkpoint_base)
    );
    println!("warm-index-run-objects {}", warm_stats.index_run_objects);
    print_wal_stats("cold-open", cold_open_wal_stats);
    print_wal_stats("cold-first-snapshot", cold_first_snapshot_wal_stats);
    print_wal_stats("cold-repeat-snapshot", cold_repeat_snapshot_wal_stats);
    print_wal_stats("warm-open", warm_open_wal_stats);
    print_wal_stats("warm-first-snapshot", warm_first_snapshot_wal_stats);
    print_wal_stats("warm-repeat-snapshot", warm_repeat_snapshot_wal_stats);
    Ok(())
}
