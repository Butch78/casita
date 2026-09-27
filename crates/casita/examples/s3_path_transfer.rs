//! Benchmark helper for `benchmark run s3-path-transfer`.
//!
//! Repository construction and import happen outside the timed region. The
//! helper then measures the same path-selected transfer twice through one
//! real S3/wal3 source handle: first with a cold pack cache and then with the
//! cache state left by the first transfer. Stable key/value output lets the
//! Python matrix driver retain exact request amplification beside latency.

#[cfg(not(all(feature = "s3", feature = "ssh")))]
fn main() {
    eprintln!("s3_path_transfer requires --features s3,ssh");
    std::process::exit(2);
}

#[cfg(all(feature = "s3", feature = "ssh"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use casita::experimental::object_store::aws::AmazonS3Builder;
    use casita::experimental::object_store::path::Path;
    use casita::experimental::{
        ChunkedBlobStore, ClosureStatus, HeldSession, MemoryBlobStore, MemoryMetadataStore,
        MetadataStore, Node, ObjectKey, Repository, RequestedStatus, RootName, TransferOptions,
        TransferReadSession, Wal3MetadataStore, Wal3ReadStats, connect_transfer_stdio_source,
        serve_transfer_stdio, transfer_path,
    };
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::sync::mpsc;

    type S3Repository = Repository<ChunkedBlobStore, Wal3MetadataStore>;

    struct RemoteTarget {
        bucket: String,
        prefix: String,
    }

    async fn open_repository(
        storage: Arc<chroma_storage::Storage>,
        bucket: &str,
        prefix: &str,
        writer: &str,
        pack_target: u64,
        pack_cache: u64,
    ) -> Result<S3Repository, Box<dyn std::error::Error + Send + Sync>> {
        let objects = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_access_key_id("minio")
            .with_secret_access_key("minio123")
            .with_region("us-east-1")
            .with_endpoint("http://127.0.0.1:9000")
            .with_allow_http(true)
            .with_checksum_algorithm(casita::experimental::object_store::aws::Checksum::SHA256)
            .build()?;
        let payloads = ChunkedBlobStore::packed_with_options(
            Arc::new(objects),
            Path::from(format!("{prefix}/payloads")),
            casita::experimental::DEFAULT_AVG_CHUNK_SIZE,
            casita::experimental::PackOptions {
                target_size: pack_target,
                cache_capacity: pack_cache,
            },
        );
        let state = Wal3MetadataStore::open(storage, format!("{prefix}/state"), writer);
        let (payloads, state) = tokio::join!(payloads, state);
        Ok(Repository::new(payloads?, state?))
    }

    async fn open_remote_repository(
        target: &RemoteTarget,
        writer: &str,
        pack_target: u64,
        pack_cache: u64,
    ) -> Result<S3Repository, Box<dyn std::error::Error + Send + Sync>> {
        Ok(S3Repository::s3_with_pack_options(
            &target.bucket,
            &target.prefix,
            writer,
            casita::experimental::PackOptions {
                target_size: pack_target,
                cache_capacity: pack_cache,
            },
        )
        .await?)
    }

    fn print_wal_stats(prefix: &str, stats: Wal3ReadStats) {
        println!(
            "{prefix}-wal-writer-open-requests {}",
            stats.writer_open_requests
        );
        println!(
            "{prefix}-wal-manifest-load-requests {}",
            stats.manifest_load_requests
        );
        println!(
            "{prefix}-wal-manifest-refresh-requests {}",
            stats.manifest_refresh_requests
        );
        println!(
            "{prefix}-wal-fragment-get-requests {}",
            stats.fragment_get_requests
        );
        println!(
            "{prefix}-wal-fragment-get-bytes {}",
            stats.fragment_get_bytes
        );
        println!("{prefix}-wal-cache-hits {}", stats.checkpoint_cache_hits);
        println!(
            "{prefix}-wal-cache-misses {}",
            stats.checkpoint_cache_misses
        );
        println!(
            "{prefix}-wal-logical-shard-get-requests {}",
            stats.logical_shard_get_requests
        );
        println!(
            "{prefix}-wal-logical-shard-barrier-get-requests {}",
            stats.logical_shard_barrier_get_requests
        );
        println!(
            "{prefix}-wal-logical-shard-barrier-put-requests {}",
            stats.logical_shard_barrier_put_requests
        );
    }

    async fn measure(
        label: &str,
        source: &S3Repository,
        resolver: Option<&dyn TransferReadSession>,
        source_name: &RootName,
        path: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let destination = Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new()?);
        let destination_name = RootName::try_from(format!("bench/{label}"))?;
        source.payloads().reset_pack_read_stats();
        source.metadata().reset_read_stats();

        let requests_before = resolver
            .and_then(|source| source.transport_requests())
            .unwrap_or(0);
        let started = Instant::now();
        let outcome = match resolver {
            Some(resolver) => {
                transfer_path(
                    &HeldSession(resolver),
                    &destination,
                    source_name,
                    path,
                    Some(destination_name.clone()),
                    TransferOptions::default(),
                )
                .await?
            }
            None => {
                transfer_path(
                    source,
                    &destination,
                    source_name,
                    path,
                    Some(destination_name.clone()),
                    TransferOptions::default(),
                )
                .await?
            }
        };
        let elapsed = started.elapsed();
        let node = outcome
            .node
            .ok_or("benchmark path was unexpectedly absent")?;
        let target = match node {
            Node::Directory { digest, .. } => ObjectKey::directory(digest),
            other => {
                return Err(format!("benchmark path selected {other:?}, not a directory").into());
            }
        };
        let progress = outcome
            .transfer
            .ok_or("selected directory omitted its closure transfer")?
            .progress;
        if progress.requested != vec![RequestedStatus::Complete(target.clone())] {
            return Err(format!(
                "selected closure did not complete: {:?}",
                progress.requested
            )
            .into());
        }
        if !matches!(
            destination.verify_closure(&target).await?,
            ClosureStatus::Complete { .. }
        ) {
            return Err("destination selected closure failed verification".into());
        }
        let snapshot = destination.metadata().snapshot().await?;
        if snapshot.root(&destination_name).await? != Some(target) {
            return Err("destination root does not select the verified path closure".into());
        }

        let pack = source
            .payloads()
            .pack_read_stats()
            .ok_or("S3 benchmark source did not expose pack statistics")?;
        println!(
            "{label}-rpc-requests {}",
            resolver
                .and_then(|source| source.transport_requests())
                .unwrap_or(0)
                - requests_before
        );
        println!("{label}-wall-nanos {}", elapsed.as_nanos());
        println!("{label}-published-objects {}", progress.published_objects);
        println!("{label}-payloads-sent {}", progress.payloads_sent);
        println!("{label}-chunks-sent {}", progress.chunks_sent);
        println!("{label}-pack-list-requests {}", pack.list_requests);
        println!(
            "{label}-pack-footer-range-requests {}",
            pack.footer_range_requests
        );
        println!(
            "{label}-pack-footer-range-bytes {}",
            pack.footer_range_bytes
        );
        println!(
            "{label}-pack-chunk-range-requests {}",
            pack.chunk_range_requests
        );
        println!("{label}-pack-chunk-range-bytes {}", pack.chunk_range_bytes);
        println!("{label}-pack-whole-requests {}", pack.whole_pack_requests);
        println!("{label}-pack-whole-bytes {}", pack.whole_pack_bytes);
        println!("{label}-pack-cache-hits {}", pack.cache_hits);
        println!("{label}-pack-cache-promotions {}", pack.cache_promotions);
        print_wal_stats(label, source.metadata().read_stats());
        Ok(())
    }

    async fn delayed_direction<R, W>(
        mut reader: R,
        mut writer: W,
        one_way_delay: Duration,
        bytes_per_second: u64,
    ) -> std::io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let (sender, mut receiver) = mpsc::channel::<(tokio::time::Instant, Vec<u8>)>(16);
        let read = async move {
            loop {
                let mut payload = vec![0u8; 1024 * 1024];
                let read = reader.read(&mut payload).await?;
                if read == 0 {
                    break;
                }
                payload.truncate(read);
                sender
                    .send((tokio::time::Instant::now() + one_way_delay, payload))
                    .await
                    .map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "delayed relay writer stopped",
                        )
                    })?;
            }
            Ok::<(), std::io::Error>(())
        };
        let write = async move {
            let mut next_send = tokio::time::Instant::now();
            while let Some((ready_at, payload)) = receiver.recv().await {
                let ready_at = if bytes_per_second == 0 {
                    ready_at
                } else {
                    next_send = next_send.max(ready_at)
                        + Duration::from_secs_f64(payload.len() as f64 / bytes_per_second as f64);
                    next_send
                };
                tokio::time::sleep_until(ready_at).await;
                writer.write_all(&payload).await?;
                writer.flush().await?;
            }
            writer.shutdown().await
        };
        tokio::try_join!(read, write)?;
        Ok(())
    }

    let mut args = std::env::args_os().skip(1);
    let mut first = args
        .next()
        .ok_or("missing source tree, --s3, or --resolver-rtt-ms")?;
    let resolver_rtt_ms = if first.to_str() == Some("--resolver-rtt-ms") {
        let rtt = args
            .next()
            .ok_or("missing resolver RTT")?
            .to_str()
            .ok_or("resolver RTT is not UTF-8")?
            .parse::<u64>()?;
        first = args.next().ok_or("missing source tree or --s3")?;
        Some(rtt)
    } else {
        None
    };
    let (remote, source_tree) = if first.to_str() == Some("--s3") {
        let bucket = args
            .next()
            .ok_or("missing S3 bucket")?
            .to_str()
            .ok_or("S3 bucket is not UTF-8")?
            .to_owned();
        let prefix = args
            .next()
            .ok_or("missing S3 prefix")?
            .to_str()
            .ok_or("S3 prefix is not UTF-8")?
            .to_owned();
        if bucket.is_empty() || prefix.is_empty() {
            return Err("S3 bucket and benchmark prefix must be non-empty".into());
        }
        let source_tree = PathBuf::from(args.next().ok_or("missing source tree")?);
        (Some(RemoteTarget { bucket, prefix }), source_tree)
    } else {
        (None, PathBuf::from(first))
    };
    let selected_path = args
        .next()
        .ok_or("missing selected path")?
        .to_str()
        .ok_or("selected path is not UTF-8")?
        .to_owned();
    let pack_target: u64 = args
        .next()
        .ok_or("missing pack target")?
        .to_str()
        .ok_or("pack target is not UTF-8")?
        .parse()?;
    let pack_cache: u64 = args
        .next()
        .ok_or("missing pack cache")?
        .to_str()
        .ok_or("pack cache is not UTF-8")?
        .parse()?;
    let writer = args
        .next()
        .ok_or("missing writer")?
        .to_str()
        .ok_or("writer is not UTF-8")?
        .to_owned();
    if args.next().is_some() {
        return Err("unexpected trailing argument".into());
    }

    let source_name = RootName::try_from("bench/current")?;
    let source = if let Some(target) = remote {
        let writer_repository =
            open_remote_repository(&target, &format!("{writer}-seed"), pack_target, pack_cache)
                .await?;
        writer_repository
            .import(casita::import::FilesystemImport::new(
                &source_tree,
                source_name.clone(),
            ))
            .await?;
        drop(writer_repository);
        open_remote_repository(
            &target,
            &format!("{writer}-reader"),
            pack_target,
            pack_cache,
        )
        .await?
    } else {
        let mut storage = None;
        let mut last_panic = None;
        for _ in 0..5 {
            match tokio::spawn(chroma_storage::s3_client_for_test_with_new_bucket()).await {
                Ok(created) => {
                    storage = Some(Arc::new(created));
                    break;
                }
                Err(error) if error.is_panic() => {
                    last_panic = Some(error);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    return Err(format!("RustFS bucket setup was cancelled: {error}").into());
                }
            }
        }
        let storage = storage
            .ok_or_else(|| format!("RustFS did not accept bucket creation: {last_panic:?}"))?;
        let bucket = storage
            .bucket_name()
            .ok_or("RustFS test storage has no bucket")?
            .to_owned();
        let prefix = format!("path-transfer/{writer}");
        let writer_repository = open_repository(
            storage.clone(),
            &bucket,
            &prefix,
            &format!("{writer}-seed"),
            pack_target,
            pack_cache,
        )
        .await?;
        writer_repository
            .import(casita::import::FilesystemImport::new(
                &source_tree,
                source_name.clone(),
            ))
            .await?;
        drop(writer_repository);
        open_repository(
            storage,
            &bucket,
            &prefix,
            &format!("{writer}-reader"),
            pack_target,
            pack_cache,
        )
        .await?
    };
    let bytes_per_second = std::env::var("CASITA_BENCH_BANDWIDTH_BYTES_PER_SECOND")
        .ok()
        .map(|v| v.parse::<u64>())
        .transpose()?
        .unwrap_or(0);
    if let Some(rtt_ms) = resolver_rtt_ms {
        let (client, relay_client) = tokio::io::duplex(8 * 1024 * 1024);
        let (relay_server, server) = tokio::io::duplex(8 * 1024 * 1024);
        let (relay_client_read, relay_client_write) = tokio::io::split(relay_client);
        let (relay_server_read, relay_server_write) = tokio::io::split(relay_server);
        let one_way_delay = Duration::from_micros(rtt_ms.saturating_mul(500));
        let relay = tokio::spawn(async move {
            tokio::try_join!(
                delayed_direction(
                    relay_client_read,
                    relay_server_write,
                    one_way_delay,
                    bytes_per_second
                ),
                delayed_direction(
                    relay_server_read,
                    relay_client_write,
                    one_way_delay,
                    bytes_per_second
                ),
            )?;
            Ok::<(), std::io::Error>(())
        });
        let server_source = source.clone();
        let (server_read, server_write) = tokio::io::split(server);
        let server = tokio::spawn(async move {
            serve_transfer_stdio(&server_source, server_read, server_write).await
        });
        let (client_read, client_write) = tokio::io::split(client);
        let resolver = connect_transfer_stdio_source(
            client_read,
            client_write,
            casita::experimental::TransferSelection::Selected {
                objects: Vec::new(),
                roots: vec![source_name.clone()],
            },
        )
        .await?;
        measure(
            "cold",
            &source,
            Some(&resolver),
            &source_name,
            &selected_path,
        )
        .await?;
        measure(
            "warm",
            &source,
            Some(&resolver),
            &source_name,
            &selected_path,
        )
        .await?;
        drop(resolver);
        relay.await??;
        server.await??;
    } else {
        measure("cold", &source, None, &source_name, &selected_path).await?;
        measure("warm", &source, None, &source_name, &selected_path).await?;
    }
    Ok(())
}
