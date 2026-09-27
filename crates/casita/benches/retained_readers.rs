//! Run with `benchmark run retained-readers`; every timing checks payload bytes.
use casita::experimental::{MetadataStore, Repository as CoreRepository};
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};

type Stream = Box<dyn AsyncRead + Unpin + Send>;

async fn scenario(process_owned: bool, fanout: usize, gc: bool, iterations: usize) {
    let directory = tempfile::tempdir().unwrap();
    let repo = casita::Repository::local(directory.path()).await.unwrap();
    let core = CoreRepository::local(directory.path()).await.unwrap();
    let root = "sentinel".parse().unwrap();
    let expected = vec![42; 4096];
    let key = repo
        .import(casita::import::BlobImport::new(&expected[..], root))
        .await
        .unwrap();
    repo.flush().await.unwrap();
    let pins = core.metadata().pin_store().await.unwrap();
    if process_owned {
        drop(repo.retained_reader().await.unwrap());
        repo.flush().await.unwrap();
    }
    let durable_path = directory.path().join("casita.sqlite.online-pins");
    let durable_before = std::fs::read(&durable_path).unwrap();
    let before = pins.inventory().await.unwrap().revision;
    let done = AtomicBool::new(false);
    let start = Instant::now();
    let reads = async {
        let mut admissions = Vec::new();
        let mut opens = Vec::new();
        for _ in 0..iterations {
            let admission = Instant::now();
            let mut streams: Vec<Stream> = Vec::new();
            if process_owned {
                let session = repo.retained_reader().await.unwrap();
                admissions.push(admission.elapsed().as_secs_f64());
                let opening = Instant::now();
                for _ in 0..fanout {
                    streams.push(Box::new(session.open(&key).await.unwrap().unwrap()));
                }
                opens.push(opening.elapsed().as_secs_f64());
                drop(session);
            } else {
                let session = core.owned_retention_hold().await.unwrap();
                admissions.push(admission.elapsed().as_secs_f64());
                let opening = Instant::now();
                for _ in 0..fanout {
                    let (_, stream) = session.open_payload(&key).await.unwrap().unwrap();
                    streams.push(Box::new(stream));
                }
                opens.push(opening.elapsed().as_secs_f64());
                drop(session);
            }
            // All readers must share one token; GC can retain released history.
            let inventory = pins.inventory().await.unwrap();
            assert_eq!(inventory.pins.len() - inventory.retired.len(), 1);
            for mut stream in streams {
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
            }
            repo.flush().await.unwrap();
        }
        done.store(true, Ordering::Release);
        (admissions, opens)
    };
    let collector = async {
        let mut passes = 0;
        while gc && !done.load(Ordering::Acquire) {
            repo.collect().await.unwrap();
            passes += 1;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        passes
    };
    let ((mut admissions, opens), passes) = tokio::join!(reads, collector);
    let seconds = start.elapsed().as_secs_f64();
    repo.flush().await.unwrap();
    let after = pins.inventory().await.unwrap();
    assert!(after.pins.is_empty());
    if process_owned && !gc {
        assert_eq!(std::fs::read(&durable_path).unwrap(), durable_before);
    }
    assert!(!gc || passes > 0);
    assert!(repo.fsck().await.unwrap().is_clean());
    repo.flush().await.unwrap();
    admissions.sort_by(f64::total_cmp);
    println!(
        "{}",
        json!({
            "ownership": if process_owned { "process" } else { "durable" },
            "readers_per_session": fanout, "concurrent_gc": gc, "iterations": iterations,
            "payload_bytes": expected.len(), "wall_seconds": seconds,
            "admission_p50_seconds": admissions[iterations / 2],
            "admission_p95_seconds": admissions[(iterations * 95).div_ceil(100) - 1],
            "open_seconds_per_reader": opens.iter().sum::<f64>() / (fanout * iterations) as f64,
            "ledger_revision_changes": after.revision - before, "gc_passes": passes,
            "correctness": "passed"
        })
    );
}

fn main() {
    let iterations = std::env::var("CASITA_BENCH_RETAINED_ITERATIONS")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(30);
    assert!(iterations > 0);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for process_owned in [false, true] {
                for fanout in [1, 32] {
                    for gc in [false, true] {
                        tokio::time::timeout(
                            Duration::from_secs(300),
                            scenario(process_owned, fanout, gc, iterations),
                        )
                        .await
                        .unwrap();
                    }
                }
            }
        });
}
