//! Benchmark helper for `benchmark run s3-pack-gc`.
//!
//! This is deliberately an example rather than a production command: it
//! creates one fresh RustFS bucket, publishes a base and retained tree through
//! the S3/wal3 repository profile, times GC, validates the result, and prints
//! stable key/value metrics for the Python matrix driver.

#[cfg(not(feature = "s3"))]
fn main() {
    eprintln!("pack_gc_rustfs requires --features s3");
    std::process::exit(2);
}

#[cfg(feature = "s3")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::sync::Arc;
    use std::time::Instant;

    use casita::experimental::object_store::aws::AmazonS3Builder;
    use casita::experimental::object_store::path::Path;
    use casita::experimental::{ChunkedBlobStore, Repository, RootName, Wal3MetadataStore};
    use chroma_config::Configurable;

    let mut args = std::env::args_os().skip(1);
    let base = args.next().ok_or("missing base tree")?;
    let retained = args.next().ok_or("missing retained tree")?;
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
    let prefix = format!("pack-gc/{writer}");
    let objects = AmazonS3Builder::new()
        .with_bucket_name(&bucket)
        .with_access_key_id("minio")
        .with_secret_access_key("minio123")
        .with_region("us-east-1")
        .with_endpoint(&endpoint)
        .with_allow_http(true)
        .with_checksum_algorithm(casita::experimental::object_store::aws::Checksum::SHA256)
        .build()?;
    let payloads = ChunkedBlobStore::packed_with_options(
        Arc::new(objects),
        Path::from(format!("{prefix}/payloads")),
        casita::experimental::DEFAULT_AVG_CHUNK_SIZE,
        casita::experimental::PackOptions {
            target_size: target,
            cache_capacity: 0,
        },
    )
    .await?;
    let state = Wal3MetadataStore::open(storage, format!("{prefix}/state"), writer).await?;
    let repository = Repository::new(payloads, state);
    let name = RootName::try_from("bench/current")?;
    repository
        .import(casita::import::FilesystemImport::new(base, name.clone()))
        .await?;
    let retained_root = repository
        .import(casita::import::FilesystemImport::new(retained, name))
        .await?;

    repository.payloads().reset_pack_read_stats();
    repository.metadata().reset_read_stats();
    let started = Instant::now();
    let outcome = repository.collect().await?;
    let elapsed = started.elapsed();
    let stats = repository
        .payloads()
        .pack_read_stats()
        .ok_or("S3 benchmark store did not expose pack statistics")?;
    let wal_stats = repository.metadata().read_stats();
    if stats.chunk_range_requests != 0 {
        return Err(format!(
            "GC used {} survivor range requests",
            stats.chunk_range_requests
        )
        .into());
    }
    if !repository.fsck().await?.is_clean() {
        return Err("repository failed fsck after RustFS GC".into());
    }
    if !matches!(
        repository.verify_closure(&retained_root).await?,
        casita::experimental::ClosureStatus::Complete { .. }
    ) {
        return Err("retained RustFS closure is incomplete after GC".into());
    }

    println!("gc-wall-nanos {}", elapsed.as_nanos());
    println!("removed-objects {}", outcome.removed.logical_objects);
    println!("removed-payloads {}", outcome.removed.payload_blobs);
    println!("removed-chunks {}", outcome.removed.chunks);
    println!("pack-list-requests {}", stats.list_requests);
    println!(
        "pack-gc-manifest-list-requests {}",
        stats.gc_manifest_list_requests
    );
    println!(
        "pack-gc-loose-chunk-list-requests {}",
        stats.gc_loose_chunk_list_requests
    );
    println!("pack-footer-range-requests {}", stats.footer_range_requests);
    println!("pack-footer-range-bytes {}", stats.footer_range_bytes);
    println!("pack-chunk-range-requests {}", stats.chunk_range_requests);
    println!("pack-chunk-range-bytes {}", stats.chunk_range_bytes);
    println!("pack-whole-requests {}", stats.whole_pack_requests);
    println!("pack-whole-bytes {}", stats.whole_pack_bytes);
    println!(
        "pack-gc-replacement-put-requests {}",
        stats.gc_replacement_put_requests
    );
    println!(
        "pack-gc-replacement-put-bytes {}",
        stats.gc_replacement_put_bytes
    );
    println!(
        "pack-gc-marker-put-requests {}",
        stats.gc_marker_put_requests
    );
    println!("pack-gc-marker-put-bytes {}", stats.gc_marker_put_bytes);
    println!("pack-gc-delete-requests {}", stats.gc_pack_delete_requests);
    println!(
        "pack-gc-manifest-delete-requests {}",
        stats.gc_manifest_delete_requests
    );
    println!(
        "pack-gc-outboard-delete-requests {}",
        stats.gc_outboard_delete_requests
    );
    println!(
        "pack-gc-loose-chunk-delete-requests {}",
        stats.gc_loose_chunk_delete_requests
    );
    println!(
        "pack-gc-tombstone-put-requests {}",
        stats.gc_tombstone_put_requests
    );
    println!(
        "pack-gc-tombstone-put-bytes {}",
        stats.gc_tombstone_put_bytes
    );
    println!(
        "pack-gc-tombstone-delete-requests {}",
        stats.gc_tombstone_delete_requests
    );
    println!("pack-gc-deferred-packs {}", stats.gc_deferred_packs);
    println!(
        "pack-index-pointer-requests {}",
        stats.index_pointer_requests
    );
    println!("pack-index-requests {}", stats.index_requests);
    println!("pack-index-put-requests {}", stats.index_put_requests);
    println!(
        "gc-wal-writer-open-requests {}",
        wal_stats.writer_open_requests
    );
    println!(
        "gc-wal-manifest-load-requests {}",
        wal_stats.manifest_load_requests
    );
    println!(
        "gc-wal-manifest-refresh-requests {}",
        wal_stats.manifest_refresh_requests
    );
    println!(
        "gc-wal-fragment-get-requests {}",
        wal_stats.fragment_get_requests
    );
    println!(
        "gc-wal-fragment-put-requests {}",
        wal_stats.fragment_put_requests
    );
    println!(
        "gc-wal-manifest-put-requests {}",
        wal_stats.manifest_put_requests
    );
    println!(
        "gc-wal-logical-shard-get-requests {}",
        wal_stats.logical_shard_get_requests
    );
    println!(
        "gc-wal-logical-shard-put-requests {}",
        wal_stats.logical_shard_put_requests
    );
    println!(
        "gc-wal-logical-shard-barrier-get-requests {}",
        wal_stats.logical_shard_barrier_get_requests
    );
    println!(
        "gc-wal-logical-shard-barrier-put-requests {}",
        wal_stats.logical_shard_barrier_put_requests
    );
    println!(
        "gc-wal-logical-shard-inventory-list-requests {}",
        wal_stats.logical_shard_inventory_list_requests
    );
    println!(
        "gc-wal-logical-shard-delete-requests {}",
        wal_stats.logical_shard_delete_requests
    );
    Ok(())
}
