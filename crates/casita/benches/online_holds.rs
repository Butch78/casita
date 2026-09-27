//! Reproducible local import/read/GC contention experiment; emits JSON lines.
mod bench_util;
#[path = "bench_util/gc_timing.rs"]
mod gc_timing;

use casita::{
    RetryDisposition,
    experimental::{MetadataStore, Repository, RepositoryError, flush_repository_leases},
    import::FilesystemImport,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;

fn latency(samples: &mut [f64]) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    let percentile = |p: usize| {
        samples
            .get((samples.len() * p).div_ceil(100).saturating_sub(1))
            .copied()
    };
    json!({"count": samples.len(), "p50_ms": percentile(50), "p95_ms": percentile(95), "p99_ms": percentile(99)})
}

fn scenario_name(readers: bool, gc: bool) -> &'static str {
    match (readers, gc) {
        (false, false) => "imports",
        (true, false) => "imports_readers",
        (false, true) => "imports_gc",
        (true, true) => "imports_readers_gc",
    }
}

async fn scenario(
    imports: usize,
    files: usize,
    readers: bool,
    gc: bool,
    reader_scope: &str,
    timings: &gc_timing::Timings,
) {
    let temp = tempfile::tempdir().unwrap();
    let repo = Repository::local(temp.path().join("repo")).await.unwrap();
    let application = if reader_scope == "application" && readers {
        Some(
            casita::Repository::local(temp.path().join("repo"))
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let corpus = temp.path().join("corpus");
    // Unique deterministic data prevents unchanged-import and dedup fast paths.
    for iteration in 0..imports {
        let path = corpus.join(iteration.to_string());
        std::fs::create_dir_all(&path).unwrap();
        for file in 0..files {
            std::fs::write(
                path.join(file.to_string()),
                bench_util::random_bytes((iteration * files + file) as u64, 4096),
            )
            .unwrap();
        }
    }
    let session = repo.mutation_session().await.unwrap();
    let staged = session.stage_blob(b"reader sentinel").await.unwrap();
    let key = staged.record().key().clone();
    session
        .publish_rooted(vec![staged], "sentinel".parse().unwrap(), key.clone())
        .await
        .unwrap();
    drop(session);
    flush_repository_leases().await.unwrap();
    let ledger = repo.metadata().pin_store().await.unwrap();
    let before = ledger.inventory().await.unwrap().revision;
    let done = AtomicBool::new(false);
    timings.reset();
    let start = Instant::now();
    let writer = async {
        let mut admission = Vec::new();
        let mut latest = None;
        for iteration in 0..imports {
            let start = Instant::now();
            let session = repo.mutation_session().await.unwrap();
            admission.push(start.elapsed().as_secs_f64() * 1000.0);
            latest = Some(
                session
                    .import(
                        FilesystemImport::new(
                            corpus.join(iteration.to_string()),
                            "import".parse().unwrap(),
                        )
                        .with_file_concurrency(std::num::NonZeroUsize::new(16).unwrap())
                        .reread(true),
                    )
                    .await
                    .unwrap(),
            );
        }
        let seconds = start.elapsed().as_secs_f64();
        done.store(true, Ordering::Release);
        (seconds, admission, latest.unwrap())
    };
    let reader = async {
        let mut admission = Vec::new();
        while readers && !done.load(Ordering::Acquire) {
            let start = Instant::now();
            let mut stream: Box<dyn tokio::io::AsyncRead + Send + Unpin> =
                if let Some(application) = &application {
                    Box::new(application.open(&key).await.unwrap().unwrap())
                } else if reader_scope == "snapshot" {
                    let hold = repo.retention_hold().await.unwrap();
                    hold.open_payload(&key).await.unwrap().unwrap().1
                } else {
                    repo.open_payload(&key).await.unwrap().unwrap().1
                };
            admission.push(start.elapsed().as_secs_f64() * 1000.0);
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"reader sentinel");
            drop(stream);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        admission
    };
    let collector = async {
        let (mut busy, mut retries) = (0, 0);
        let mut completed = Vec::new();
        let mut attempts = Vec::new();
        let mut busy_reasons = BTreeMap::<String, usize>::new();
        let mut retryable_reasons = BTreeMap::<String, usize>::new();
        while gc && !done.load(Ordering::Acquire) {
            timings.clear();
            let started = start.elapsed().as_secs_f64();
            let result = repo.try_collect().await;
            attempts.push(json!({
                "started_seconds": started,
                "finished_seconds": start.elapsed().as_secs_f64(),
                "result": match &result {
                    Ok(_) => "ok".to_owned(),
                    Err(error) => error.to_string(),
                },
                "phases": timings.take(start),
            }));
            match result {
                Ok(report) => {
                    completed.push((
                        start.elapsed().as_secs_f64(),
                        report.removed.logical_objects,
                    ));
                }
                Err(RepositoryError::Busy(reason)) => {
                    busy += 1;
                    *busy_reasons.entry(reason).or_default() += 1;
                }
                Err(error) if error.retry_disposition() == RetryDisposition::Retry => {
                    retries += 1;
                    *retryable_reasons.entry(error.to_string()).or_default() += 1;
                }
                Err(error) => panic!("collection failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        (
            completed,
            busy,
            retries,
            busy_reasons,
            retryable_reasons,
            attempts,
        )
    };
    let (
        (seconds, mut writes, latest),
        mut reads,
        (completed, busy, retries, busy_reasons, retryable_reasons, attempts),
    ) = tokio::join!(writer, reader, collector);
    // Classify by completion timestamp against the writer's completion timestamp,
    // not the time a pass started or the time the join returned.
    let passes = completed.len();
    let removed: usize = completed.iter().map(|(_, count)| count).sum();
    let active: Vec<_> = completed.iter().filter(|(at, _)| *at <= seconds).collect();
    let active_removed: usize = active.iter().map(|(_, count)| count).sum();
    flush_repository_leases().await.unwrap();
    let after = ledger.inventory().await.unwrap();
    if readers {
        assert!(!reads.is_empty());
    }
    if gc {
        assert!(passes + busy + retries > 0);
    }
    let revisions = after.revision - before;
    let ledger_timings = timings.take_ledger();
    // Integrity and final reclamation are outside the measured interval.
    let final_gc = repo.collect().await.unwrap();
    flush_repository_leases().await.unwrap();
    // A retryable pass can retain claims and retired pins for recovery.
    let recovered = ledger.inventory().await.unwrap();
    assert!(recovered.pins.is_empty(), "{recovered:?}");
    assert!(recovered.deletions.is_empty());
    assert!(recovered.collector.is_none() && recovered.logical_prune.is_none());
    assert!(repo.fsck().await.unwrap().is_healthy());
    flush_repository_leases().await.unwrap();
    let checkout = temp.path().join("checkout");
    repo.checkout(&latest, &checkout).await.unwrap();
    for file in 0..files {
        assert_eq!(
            std::fs::read(checkout.join(file.to_string())).unwrap(),
            bench_util::random_bytes(((imports - 1) * files + file) as u64, 4096)
        );
    }
    flush_repository_leases().await.unwrap();
    println!(
        "{}",
        json!({
            "scenario": scenario_name(readers, gc),
            "imports": imports, "files_per_import": files, "bytes_per_file": 4096,
            "import_seconds": seconds, "files_per_second": (imports * files) as f64 / seconds,
            "writer_admission": latency(&mut writes), "reader_open": latency(&mut reads),
            "reader_scope": reader_scope,
            "ledger_revision_changes": revisions, "ledger_revisions_per_file": revisions as f64 / (imports * files) as f64,
            "gc_passes": passes, "gc_busy": busy, "gc_retryable_errors": retries, "gc_removed_objects": removed,
            "gc_attempts": attempts,
            "ledger_timings": ledger_timings,
            "gc_completed_passes": completed.iter().map(|(at, count)| json!({
                "elapsed_seconds": at, "removed_objects": count,
            })).collect::<Vec<_>>(),
            "gc_busy_reasons": busy_reasons,
            "gc_retryable_error_reasons": retryable_reasons,
            "gc_passes_during_imports": active.len(),
            "gc_removed_objects_during_imports": active_removed,
            "gc_passes_after_imports": passes - active.len(),
            "gc_removed_objects_after_imports": removed - active_removed,
            "cleanup_removed_objects": final_gc.removed.logical_objects,
        })
    );
}

fn main() {
    let timings = gc_timing::Timings::install();
    let count = |name: &str, default: usize| {
        std::env::var(name)
            .map(|v| v.parse::<usize>().expect("positive integer"))
            .unwrap_or(default)
    };
    let reader_scope = match std::env::var("CASITA_BENCH_READER_SCOPE").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("application") => "application",
        Ok("object") => "object",
        Ok("snapshot") => "snapshot",
        _ => panic!("CASITA_BENCH_READER_SCOPE must be application, object, or snapshot"),
    };
    let selected = std::env::var("CASITA_BENCH_SCENARIO").ok();
    assert!(
        selected.as_deref().is_none_or(|name| [
            "imports",
            "imports_readers",
            "imports_gc",
            "imports_readers_gc",
        ]
        .contains(&name)),
        "invalid CASITA_BENCH_SCENARIO"
    );
    let imports = count("CASITA_BENCH_IMPORTS", 30);
    let files = count("CASITA_BENCH_FILES", 16);
    assert!(imports >= 2 && files > 0);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        for (readers, gc) in [(false, false), (true, false), (false, true), (true, true)] {
            if selected
                .as_deref()
                .is_some_and(|name| name != scenario_name(readers, gc))
            {
                continue;
            }
            tokio::time::timeout(
                Duration::from_secs(300),
                scenario(imports, files, readers, gc, reader_scope, &timings),
            )
            .await
            .expect("scenario exceeded five minutes");
        }
    });
}
