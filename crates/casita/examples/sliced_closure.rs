//! Sync many rebuilt store paths as one transfer over a shaped link.
//!
//! ```text
//! sliced_closure --pairs FILE --work DIR [--max-bytes N] [--max-pairs N]
//!                [--bandwidth-kib N] [--rtt-ms M]
//! ```
//!
//! Each line of `FILE` is `name base rebuilt`, as the corpus suite's closure
//! pairing produces. The base paths are imported into a source repository and
//! into a destination repository under one root each; the rebuilt paths are
//! imported into the source under those same roots. One transfer request then
//! moves every rebuilt root at once, so the round trips are those of a whole
//! closure rather than of one path repeated. A second destination that holds
//! nothing gives the cold comparison. Repositories are on disk, so the
//! measurement is not bounded by memory.

#[path = "../benches/bench_util/link.rs"]
mod link;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use casita::experimental::{
    ChunkedBlobStore, ClosureStatus, DestinationRoot, HeldSession, MetadataStore, ObjectKey,
    ObjectRequest, Repository, RootName, TransferOptions, TransferReadSession, TransferRequest,
    TransferSelection, TursoMetadataStore, connect_transfer_stdio_source, serve_transfer_stdio,
    transfer,
};
use casita::import::FilesystemImport;
use serde_json::json;

type Local = Repository<ChunkedBlobStore, TursoMetadataStore>;

struct Pair {
    base: PathBuf,
    rebuilt: PathBuf,
    bytes: u64,
}

fn tree_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len();
            }
        }
    }
    total
}

async fn import(repository: &Local, path: &Path, root: &RootName) -> ObjectKey {
    repository
        .import(FilesystemImport::new(path, root.clone()))
        .await
        .expect("filesystem import")
}

/// One transfer of every rebuilt root, with the bytes the server sent.
async fn sync(
    source: Arc<Local>,
    destination: &Local,
    request: TransferRequest,
    rtt: Duration,
    bytes_per_second: u64,
) -> serde_json::Value {
    let targets: Vec<ObjectKey> = request
        .objects
        .iter()
        .map(|object| object.key.clone())
        .collect();
    // Pack read counters say whether the serving side kept its read-ahead or
    // fell back to demand fetches because the shared buffer budget was full.
    source.payloads().reset_pack_read_stats();
    let serving = source.clone();
    let started = Instant::now();
    let shaped = link::shaped_link(rtt, bytes_per_second);
    // Report the link counters while the transfer runs: if they stop moving
    // the transfer is stuck, and the direction that moved last says which
    // side is waiting for the other.
    let (up, down) = (
        shaped.client_to_server.clone(),
        shaped.server_to_client.clone(),
    );
    let monitor = tokio::spawn(async move {
        let (mut last_up, mut last_down, mut idle) = (0, 0, 0);
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let (now_up, now_down) = (up.load(Ordering::Relaxed), down.load(Ordering::Relaxed));
            if (now_up, now_down) == (last_up, last_down) {
                idle += 5;
                eprintln!("link idle {idle}s at up={now_up} down={now_down}");
            } else {
                idle = 0;
            }
            (last_up, last_down) = (now_up, now_down);
        }
    });
    let (server_read, server_write) = tokio::io::split(shaped.server);
    let server =
        tokio::spawn(
            async move { serve_transfer_stdio(&*source, server_read, server_write).await },
        );
    let (client_read, client_write) = tokio::io::split(shaped.client);
    let session =
        connect_transfer_stdio_source(client_read, client_write, TransferSelection::Snapshot)
            .await
            .expect("connect");
    let result = transfer(
        &HeldSession(&session),
        destination,
        request,
        TransferOptions::default(),
    )
    .await
    .expect("transfer");
    let requests = session.transport_requests().unwrap_or(0);
    let operations: serde_json::Map<String, serde_json::Value> = session
        .transport_operations()
        .unwrap_or_default()
        .into_iter()
        .map(|(name, count)| (name, json!(count)))
        .collect();
    drop(session);
    server.await.expect("server task").expect("server");
    shaped.relay.await.expect("relay task").expect("relay");
    monitor.abort();
    let wall = started.elapsed();
    // Verifying every closure costs more than the transfer it checks; an
    // evenly spaced sample of at most 32 still fails on a broken transfer.
    let stride = targets.len().div_ceil(32).max(1);
    for target in targets.iter().step_by(stride) {
        assert!(
            matches!(
                destination.verify_closure(target).await.expect("verify"),
                ClosureStatus::Complete { .. }
            ),
            "destination closure incomplete at {target}"
        );
    }
    let progress = result.progress;
    let reads = serving.payloads().pack_read_stats().unwrap_or_default();
    json!({
        "pack_chunk_range_requests": reads.chunk_range_requests,
        "pack_readahead_deferrals": reads.readahead_deferrals,
        "pack_buffer_bypasses": reads.buffer_bypasses,
        "rtt_ms": rtt.as_secs_f64() * 1000.0,
        "bandwidth_kib": bytes_per_second / 1024,
        "wall_seconds": wall.as_secs_f64(),
        "round_trip_depth": wall.as_secs_f64() / rtt.as_secs_f64().max(f64::MIN_POSITIVE),
        "server_to_client_bytes": shaped.server_to_client.load(Ordering::Relaxed),
        "client_to_server_bytes": shaped.client_to_server.load(Ordering::Relaxed),
        "transport_requests": requests,
        "transport_operations": operations,
        "published_objects": progress.published_objects,
        "payloads_sent": progress.payloads_sent,
        "payloads_reused": progress.payloads_reused,
        "slice_copy_bytes": progress.slice_copy_bytes,
        "slice_literal_bytes": progress.slice_literal_bytes,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut pairs_file = None;
    let mut work = None;
    let mut max_bytes = 1024u64 * 1024 * 1024;
    let mut max_pairs = 2000usize;
    let mut bandwidth_kib = 0u64;
    let mut rtt_ms = 0u64;
    let mut skip_cold = false;
    let mut reuse = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--pairs" => pairs_file = Some(PathBuf::from(value)),
            "--work" => work = Some(PathBuf::from(value)),
            "--max-bytes" => max_bytes = value.parse().expect("byte budget"),
            "--max-pairs" => max_pairs = value.parse().expect("pair budget"),
            "--bandwidth-kib" => bandwidth_kib = value.parse().expect("bandwidth in KiB/s"),
            "--rtt-ms" => rtt_ms = value.parse().expect("round trip in ms"),
            "--skip-cold" => skip_cold = value == "true",
            "--reuse" => reuse = value == "true",
            other => panic!("unknown flag {other}"),
        }
    }
    let pairs_file = pairs_file.expect("--pairs FILE");
    let work = work.expect("--work DIR");
    let rtt = Duration::from_millis(rtt_ms);
    let bytes_per_second = bandwidth_kib * 1024;

    // Closure order, which is arbitrary with respect to size, so the sample
    // keeps the closure's own mix of large and small paths.
    let candidates: Vec<Pair> = std::fs::read_to_string(&pairs_file)
        .expect("pairs file")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _name = fields.next()?;
            let base = PathBuf::from(fields.next()?);
            let rebuilt = PathBuf::from(fields.next()?);
            // A store path may be one regular file; only trees import under
            // a root, and they are what a closure is mostly made of.
            // symlink_metadata, not is_dir: a store path that is a symlink to
            // a directory passes a following check and then fails the
            // importer's no-follow open.
            let real_dir =
                |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
            if !real_dir(&base) || !real_dir(&rebuilt) {
                return None;
            }
            let bytes = tree_bytes(&rebuilt);
            Some(Pair {
                base,
                rebuilt,
                bytes,
            })
        })
        .collect();
    let mut pairs = Vec::new();
    let mut selected_bytes = 0u64;
    for pair in candidates {
        if pairs.len() == max_pairs {
            break;
        }
        // Skip rather than stop, so one large path does not end selection.
        if selected_bytes + pair.bytes > max_bytes {
            continue;
        }
        selected_bytes += pair.bytes;
        pairs.push(pair);
    }
    assert!(!pairs.is_empty(), "no pairs fit the budget");
    eprintln!(
        "selected {} pairs, {:.2} GiB, largest {:.2} MiB",
        pairs.len(),
        selected_bytes as f64 / (1 << 30) as f64,
        pairs.iter().map(|pair| pair.bytes).max().unwrap_or(0) as f64 / (1 << 20) as f64
    );

    std::fs::create_dir_all(&work).expect("work directory");
    let source_dir = work.join("source");
    let base_dir = work.join("base");
    // Importing a closure costs far more than syncing it, so a prepared work
    // directory can be reused: the source and the base generation are never
    // mutated by a sync, and each phase runs against a fresh copy.
    let reuse = reuse && source_dir.join("casita.sqlite").exists();
    let source = Repository::local(&source_dir)
        .await
        .expect("source repository");

    let mut request = TransferRequest::default();
    if reuse {
        let snapshot = source.metadata().snapshot().await.expect("snapshot");
        let mut index = 0usize;
        loop {
            let name = RootName::try_from(format!("p/{index}")).expect("root name");
            let Some(key) = snapshot.root(&name).await.expect("root") else {
                break;
            };
            request.objects.push(ObjectRequest {
                key: key.clone(),
                recursive: true,
            });
            request.roots.push(DestinationRoot { name, target: key });
            index += 1;
        }
        assert!(
            index > 0,
            "--reuse found no imported roots under {source_dir:?}"
        );
        eprintln!("reusing {index} imported pairs");
    } else {
        let base = Repository::local(&base_dir).await.expect("base repository");
        for (index, pair) in pairs.iter().enumerate() {
            if index % 100 == 0 {
                eprintln!("importing {index}/{}", pairs.len());
            }
            let root = RootName::try_from(format!("p/{index}")).expect("root name");
            let base_root = RootName::try_from(format!("b/{index}")).expect("root name");
            // The source keeps the old content so it can slice against it,
            // under its own root; the base generation keeps it under the root
            // the transfer will replace.
            import(&source, &pair.base, &base_root).await;
            import(&base, &pair.base, &root).await;
            let rebuilt = import(&source, &pair.rebuilt, &root).await;
            request.objects.push(ObjectRequest {
                key: rebuilt.clone(),
                recursive: true,
            });
            request.roots.push(DestinationRoot {
                name: root,
                target: rebuilt,
            });
        }
    }
    let requested = request.roots.len();
    let source = Arc::new(source);

    let phases: &[&str] = if skip_cold {
        &["rebuild"]
    } else {
        &["cold", "rebuild"]
    };
    for phase in phases {
        // A fresh destination each time: empty for the cold phase, a copy of
        // the base generation for the rebuild, so phases and repeated runs
        // never see a destination another one already filled.
        let destination_dir = work.join(format!("destination-{phase}"));
        let _ = std::fs::remove_dir_all(&destination_dir);
        if *phase == "rebuild" {
            let copied = std::process::Command::new("cp")
                .arg("-a")
                .arg(&base_dir)
                .arg(&destination_dir)
                .status()
                .expect("copy the base generation");
            assert!(copied.success(), "copying the base generation failed");
        } else {
            std::fs::create_dir_all(&destination_dir).expect("destination directory");
        }
        let destination = Repository::local(&destination_dir)
            .await
            .expect("destination repository");
        eprintln!("syncing {phase}");
        let mut row = sync(
            source.clone(),
            &destination,
            request.clone(),
            rtt,
            bytes_per_second,
        )
        .await;
        row["schema"] = json!("casita.sliced-closure.v1");
        row["phase"] = json!(phase);
        row["pairs"] = json!(requested);
        row["logical_bytes"] = json!(selected_bytes);
        row["correctness"] = json!("passed");
        println!("{row}");
    }
}
