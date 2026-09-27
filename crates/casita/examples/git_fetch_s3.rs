//! S3-backed Git HTTP fixture for `benchmark run git-fetch-s3`.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use casita::experimental::*;
use casita::import::GitImport;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

#[derive(Clone, Default)]
struct Timings(Arc<Mutex<std::collections::BTreeMap<&'static str, (u64, f64)>>>);

impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Timings {
    fn on_new_span(
        &self,
        _: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).unwrap();
        if matches!(
            span.name(),
            "git.fetch.write_pack" | "git.fetch.streaming_entry"
        ) {
            span.extensions_mut().insert(Instant::now());
        }
    }

    fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).unwrap();
        if let Some(start) = span.extensions().get::<Instant>() {
            let mut totals = self.0.lock().unwrap();
            let entry = totals.entry(span.name()).or_default();
            entry.0 += 1;
            entry.1 += start.elapsed().as_secs_f64();
        }
    }
}

fn sample<SS: MetadataStore>(
    repository: &Repository<ChunkedBlobStore, SS>,
    timings: &Timings,
    event: &str,
) {
    let stats = repository.payloads().pack_read_stats().unwrap();
    println!(
        "{}",
        serde_json::json!({
            "event": event,
            "chunk_range_requests": stats.chunk_range_requests,
            "chunk_range_bytes": stats.chunk_range_bytes,
            "whole_pack_requests": stats.whole_pack_requests,
            "whole_pack_bytes": stats.whole_pack_bytes,
            "cache_hits": stats.cache_hits,
            "cache_evictions": stats.cache_evictions,
            "span_totals": *timings.0.lock().unwrap(),
        })
    );
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let timings = Timings::default();
    if std::env::var_os("CASITA_BENCH_DIAGNOSTICS").is_some() {
        tracing_subscriber::registry()
            .with(
                timings
                    .clone()
                    .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                        matches!(
                            metadata.name(),
                            "git.fetch.write_pack" | "git.fetch.streaming_entry"
                        )
                    })),
            )
            .init();
    }
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        6,
        "MODE PREFIX SOURCE_OR_READY_FILE CACHE_BYTES VIEW"
    );
    if std::env::var("CASITA_BENCH_BACKEND").as_deref() == Ok("local") {
        let repository = Repository::local_with_pack_options(
            &args[2],
            casita::experimental::PackOptions {
                target_size: 4 * 1024 * 1024,
                cache_capacity: args[4].parse()?,
            },
        )
        .await?;
        run(repository, args, timings).await
    } else {
        let repository = Repository::s3_with_pack_options(
            "casita-git-fetch",
            &args[2],
            format!("fetch-bench-{}", std::process::id()),
            casita::experimental::PackOptions {
                target_size: 4 * 1024 * 1024,
                cache_capacity: args[4].parse()?,
            },
        )
        .await?;
        run(repository, args, timings).await
    }
}

async fn run<SS: MetadataStore + Clone + 'static>(
    repository: Repository<ChunkedBlobStore, SS>,
    args: Vec<String>,
    timings: Timings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if matches!(
        args[1].as_str(),
        "import" | "import-local" | "import-prepared"
    ) {
        let start = Instant::now();
        let input = GitImport::new(&args[3], &args[5])
            .with_refs(["refs/heads/main"])?
            .with_max_cached_pack_bytes(0);
        let objects = if args[1] != "import" {
            let temp = (args[1] == "import-local")
                .then(tempfile::tempdir)
                .transpose()?;
            let local_path = temp
                .as_ref()
                .map(|temp| temp.path())
                .unwrap_or_else(|| std::path::Path::new(&args[3]));
            let local = Repository::local(local_path).await?;
            if temp.is_some() {
                local.import(input).await?;
            }
            let (view_key, view) = read_git_view(&local, &args[5])
                .await?
                .ok_or("missing prepared view")?;
            assert!(
                view.pack.is_none(),
                "fixture must not contain a cached native Git pack"
            );
            let main_ref = CanonicalRefName::try_from("refs/heads/main")?;
            let GitRefValue::Direct(tip) = &view.refs[&main_ref] else {
                return Err("fixture main must be direct".into());
            };
            if let Ok(expected) = std::env::var("CASITA_BENCH_EXPECTED_COMMIT") {
                assert_eq!(
                    tip.native_id(),
                    gix_hash::ObjectId::from_hex(expected.as_bytes())?.as_bytes()
                );
            }
            let objects = view.objects.len();
            eprintln!("local import complete: {objects} objects");
            let result = transfer(
                &local,
                &repository,
                TransferRequest {
                    objects: vec![ObjectRequest {
                        key: view_key.clone(),
                        recursive: true,
                    }],
                    roots: vec![DestinationRoot {
                        name: git_view_root_name(&args[5])?,
                        target: view_key.clone(),
                    }],
                },
                TransferOptions::default(),
            )
            .await?;
            assert_eq!(
                result.progress.requested,
                vec![RequestedStatus::Complete(view_key)]
            );
            drop(local);
            flush_repository_leases().await?;
            objects
        } else {
            let concurrency = std::env::var("CASITA_BENCH_IMPORT_CONCURRENCY")
                .unwrap_or_else(|_| "1".into())
                .parse::<std::num::NonZeroUsize>()?;
            repository
                .import(input.with_concurrency(concurrency))
                .await?
                .objects
        };
        println!(
            "{}",
            serde_json::json!({"objects": objects, "import_seconds": start.elapsed().as_secs_f64()})
        );
    } else {
        assert_eq!(args[1], "serve");
        let (_, view) = read_git_view(&repository, &args[5])
            .await?
            .ok_or("missing view")?;
        assert!(
            view.pack.is_none(),
            "fixture must not contain a cached native Git pack"
        );
        if let Ok(expected) = std::env::var("CASITA_BENCH_EXPECTED_COMMIT") {
            let GitRefValue::Direct(tip) =
                &view.refs[&CanonicalRefName::try_from("refs/heads/main")?]
            else {
                return Err("fixture main must be direct".into());
            };
            assert_eq!(
                tip.native_id(),
                gix_hash::ObjectId::from_hex(expected.as_bytes())?.as_bytes()
            );
        }
        let mut limits = GitFetchLimits::default();
        limits.max_pack_bytes = 8 * 1024 * 1024 * 1024;
        let service = GitFetchService::bind(&repository, &args[5], limits).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let ready = PathBuf::from(&args[3]);
        std::fs::write(
            &ready,
            format!("http://{}/repo.git", listener.local_addr()?),
        )?;
        repository.payloads().reset_pack_read_stats();
        let mut options = GitHttpOptions::default();
        options.total_request_timeout = Duration::from_secs(3600);
        options.pack_generation_timeout = Duration::from_secs(3600);
        serve_git_smart_http_with_shutdown(listener, "/repo.git".into(), service, options, async {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line != "stats" {
                    break;
                }
                sample(&repository, &timings, "sample");
            }
        })
        .await?;
        sample(&repository, &timings, "shutdown");
    }
    drop(repository);
    flush_repository_leases().await?;
    Ok(())
}
