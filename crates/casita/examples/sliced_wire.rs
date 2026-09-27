//! Sync a rebuilt tree over a shaped link, cold and against its base.
//!
//! ```text
//! sliced_wire --base DIR --rebuilt DIR [--bandwidth-kib N] [--rtt-ms M]
//! ```
//!
//! The base tree is imported into a source and a destination repository, the
//! rebuilt tree into the source under the same root. Two syncs of the rebuilt
//! root run through the in-process stdio transfer protocol behind a relay
//! that delays each direction by half the round trip and paces it at the
//! bandwidth: one into an empty destination, one into the destination that
//! holds the base. Each prints one JSON line with wire bytes per direction,
//! copy and literal bytes, transport requests, and wall time, after the
//! destination closure verified complete.

#[path = "../benches/bench_util/link.rs"]
mod link;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use casita::experimental::{
    ChunkedBlobStore, ClosureStatus, DEFAULT_AVG_CHUNK_SIZE, DestinationRoot, HeldSession,
    MemoryMetadataStore, MetadataStore, ObjectKey, ObjectRequest, Repository, RootName,
    TransferOptions, TransferReadSession, TransferRequest, TransferSelection,
    connect_transfer_stdio_source, serve_transfer_stdio, transfer,
};
use casita::import::FilesystemImport;
use object_store::{memory::InMemory, path::Path};
use serde_json::json;

type Memory = Repository<ChunkedBlobStore, MemoryMetadataStore>;

fn repository() -> Memory {
    let backend = Arc::new(InMemory::new());
    Repository::new(
        ChunkedBlobStore::new(backend, Path::default(), DEFAULT_AVG_CHUNK_SIZE),
        MemoryMetadataStore::new().expect("in-memory metadata"),
    )
}

async fn import(repository: &Memory, path: &PathBuf, name: &RootName) -> ObjectKey {
    repository
        .import(FilesystemImport::new(path, name.clone()))
        .await
        .expect("filesystem import")
}

async fn sync(
    source: Arc<Memory>,
    destination: &Memory,
    name: &RootName,
    target: &ObjectKey,
    rtt: Duration,
    bytes_per_second: u64,
) -> serde_json::Value {
    let started = Instant::now();
    let link = link::shaped_link(rtt, bytes_per_second);
    let (server_read, server_write) = tokio::io::split(link.server);
    let server =
        tokio::spawn(
            async move { serve_transfer_stdio(&*source, server_read, server_write).await },
        );
    let (client_read, client_write) = tokio::io::split(link.client);
    let session =
        connect_transfer_stdio_source(client_read, client_write, TransferSelection::Snapshot)
            .await
            .expect("connect");
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
    link.relay.await.expect("relay task").expect("relay");
    let wall = started.elapsed();
    assert!(
        matches!(
            destination.verify_closure(target).await.expect("verify"),
            ClosureStatus::Complete { .. }
        ),
        "destination closure incomplete"
    );
    let progress = result.progress;
    json!({
        "rtt_ms": rtt.as_secs_f64() * 1000.0,
        "bandwidth_kib": bytes_per_second / 1024,
        "wall_seconds": wall.as_secs_f64(),
        "server_to_client_bytes": link.server_to_client.load(std::sync::atomic::Ordering::Relaxed),
        "client_to_server_bytes": link.client_to_server.load(std::sync::atomic::Ordering::Relaxed),
        "transport_requests": requests,
        "transport_operations": operations,
        "published_objects": progress.published_objects,
        "payloads_sent": progress.payloads_sent,
        "payloads_reused": progress.payloads_reused,
        "slice_copy_bytes": progress.slice_copy_bytes,
        "slice_literal_bytes": progress.slice_literal_bytes,
    })
}

fn tree_bytes(path: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read tree") {
            let entry = entry.expect("tree entry");
            let metadata = entry.metadata().expect("tree metadata");
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len();
            }
        }
    }
    total
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut base = None;
    let mut rebuilt = None;
    let mut bandwidth_kib = 0u64;
    let mut rtt_ms = 0u64;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--base" => base = Some(PathBuf::from(value)),
            "--rebuilt" => rebuilt = Some(PathBuf::from(value)),
            "--bandwidth-kib" => bandwidth_kib = value.parse().expect("bandwidth in KiB/s"),
            "--rtt-ms" => rtt_ms = value.parse().expect("round trip in ms"),
            other => panic!("unknown flag {other}"),
        }
    }
    let base = base.expect("--base DIR");
    let rebuilt = rebuilt.expect("--rebuilt DIR");
    let rtt = Duration::from_millis(rtt_ms);
    let bytes_per_second = bandwidth_kib * 1024;
    let name = RootName::try_from("system").expect("root name");

    let source = Arc::new(repository());
    import(&source, &base, &name).await;
    let held = repository();
    let base_key = import(&held, &base, &name).await;
    let rebuilt_key = import(&source, &rebuilt, &name).await;
    assert_ne!(
        base_key, rebuilt_key,
        "base and rebuilt trees are identical"
    );
    assert_eq!(
        held.metadata()
            .snapshot()
            .await
            .expect("snapshot")
            .root(&name)
            .await
            .expect("root"),
        Some(base_key)
    );
    let logical = tree_bytes(&rebuilt);

    let cold = sync(
        source.clone(),
        &repository(),
        &name,
        &rebuilt_key,
        rtt,
        bytes_per_second,
    )
    .await;
    let against_base = sync(source, &held, &name, &rebuilt_key, rtt, bytes_per_second).await;
    for (phase, row) in [("cold", cold), ("rebuild", against_base)] {
        let mut row = row;
        row["schema"] = json!("casita.sliced-wire.v1");
        row["phase"] = json!(phase);
        row["logical_bytes"] = json!(logical);
        row["base"] = json!(base.display().to_string());
        row["rebuilt"] = json!(rebuilt.display().to_string());
        row["correctness"] = json!("passed");
        println!("{row}");
    }
}
