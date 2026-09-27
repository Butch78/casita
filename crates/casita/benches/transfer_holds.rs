//! `benchmark run transfer-holds`: paired retention policies on identical data.
mod bench_util;
#[path = "bench_util/gc_timing.rs"]
mod gc_timing;

use casita::experimental::{
    HeldSession, ObjectRequest, Repository, RootChange, TransferOptions, TransferReadSession,
    TransferRequest, TransferSelection, TransferSource, connect_transfer_stdio_source,
    flush_repository_leases, serve_transfer_stdio, transfer,
};
use serde_json::json;
use std::{path::Path, time::Instant};
use tokio::io::AsyncReadExt;

fn disk_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                disk_bytes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

async fn scenario(
    size: usize,
    remote: bool,
    snapshot: bool,
    gc: bool,
    timings: &gc_timing::Timings,
) {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("source");
    let source = Repository::local(&source_path).await.unwrap();
    let destination = Repository::local(temp.path().join("destination"))
        .await
        .unwrap();
    let payload = bench_util::random_bytes(42, size);
    let name: casita::RootName = "selected".parse().unwrap();
    let mutation = source.mutation_session().await.unwrap();
    let selected = mutation.stage_blob(&payload).await.unwrap();
    let key = selected.record().key().clone();
    let mut staged = vec![selected];
    for seed in 0..16 {
        staged.push(
            mutation
                .stage_blob(&bench_util::random_bytes(seed, 256 * 1024))
                .await
                .unwrap(),
        );
    }
    mutation
        .publish(
            staged,
            vec![RootChange::Set {
                name: name.clone(),
                target: key.clone(),
            }],
        )
        .await
        .unwrap();
    drop(mutation);
    flush_repository_leases().await.unwrap();
    let selection = if snapshot {
        TransferSelection::Snapshot
    } else {
        TransferSelection::Selected {
            objects: Vec::new(),
            roots: vec![name.clone()],
        }
    };
    let acquisition = Instant::now();
    let (session, server): (Box<dyn TransferReadSession + '_>, _) = if remote {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server_source = source.clone();
        let (input, output) = tokio::io::split(server);
        let task =
            tokio::spawn(async move { serve_transfer_stdio(&server_source, input, output).await });
        let (input, output) = tokio::io::split(client);
        (
            Box::new(
                connect_transfer_stdio_source(input, output, selection)
                    .await
                    .unwrap(),
            ),
            Some(task),
        )
    } else {
        (source.begin_transfer(selection).await.unwrap(), None)
    };
    let acquisition_seconds = acquisition.elapsed().as_secs_f64();
    let revision = session.revision();
    let mutation = source.mutation_session().await.unwrap();
    mutation
        .publish(Vec::new(), vec![RootChange::Remove { name: name.clone() }])
        .await
        .unwrap();
    drop(mutation);
    flush_repository_leases().await.unwrap();

    // Pause an actual payload stream after its first byte, collect, then
    // verify the remainder. Keep this controlled overlap outside copy timing.
    let before = disk_bytes(&source_path.join("blobs/packs"));
    let mut during = None;
    if gc {
        let record = session.object(&key).await.unwrap().unwrap();
        let mut reader = session.open_payload(&record).await.unwrap().unwrap();
        let mut first = [0];
        reader.read_exact(&mut first).await.unwrap();
        timings.reset();
        let start = Instant::now();
        let collected = source.try_collect().await.unwrap();
        let seconds = start.elapsed().as_secs_f64();
        let phases = timings.take(start);
        let ledger = timings.take_ledger();
        if !snapshot {
            let syncs = ledger["journal_append_sync"]["count"].as_u64().unwrap();
            assert!(
                syncs <= 16,
                "GC must batch retired files rather than synchronizing a claim per file: {syncs}"
            );
        }
        assert_eq!(
            collected.removed.logical_objects,
            if snapshot { 0 } else { 16 }
        );
        let after = disk_bytes(&source_path.join("blobs/packs"));
        if !snapshot && size == 4 * 1024 * 1024 {
            // The historical catalog keeps its packs until this read is
            // released, even though unrelated logical objects are collected.
            assert_eq!(before, after, "the active catalog must retain its packs");
        }
        let mut received = vec![first[0]];
        reader.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload);
        during = Some(
            json!({"seconds": seconds, "logical_objects": collected.removed.logical_objects,
            "pack_bytes_before": before, "pack_bytes_after": after,
            "pack_bytes_reclaimed": i128::from(before) - i128::from(after),
            "phases": phases, "ledger": ledger}),
        );
    }
    assert_eq!(session.revision(), revision);
    assert_eq!(session.root(&name).await.unwrap(), Some(key.clone()));
    let start = Instant::now();
    transfer(
        &HeldSession(session.as_ref()),
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: key.clone(),
                recursive: true,
            }],
            roots: Vec::new(),
        },
        TransferOptions::default(),
    )
    .await
    .unwrap();
    let transfer_seconds = start.elapsed().as_secs_f64();
    let (_, mut reader) = destination.open_payload(&key).await.unwrap().unwrap();
    let mut received = Vec::new();
    reader.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, payload);
    drop(reader);
    let before_release = disk_bytes(&source_path.join("blobs/packs"));
    drop(session);
    if let Some(server) = server {
        server.await.unwrap().unwrap();
    }
    flush_repository_leases().await.unwrap();
    timings.reset();
    let release_start = Instant::now();
    let released = source.try_collect().await.unwrap();
    let release_seconds = release_start.elapsed().as_secs_f64();
    let release_phases = timings.take(release_start);
    let release_ledger = timings.take_ledger();
    assert_eq!(
        released.removed.logical_objects,
        if gc && !snapshot { 1 } else { 17 }
    );
    let remaining = disk_bytes(&source_path.join("blobs/packs"));
    assert!(before > 0, "fixture must contain physical packs");
    assert_eq!(
        remaining, 0,
        "all unrooted packs must be reclaimed after release"
    );
    println!(
        "{}",
        json!({"transport": if remote { "ssh-stdio" } else { "local" },
        "scope": if snapshot { "snapshot" } else { "selected" }, "gc": gc,
        "payload_bytes": size, "garbage_objects": 16, "garbage_bytes": 16 * 256 * 1024,
        "acquisition_seconds": acquisition_seconds, "transfer_seconds": transfer_seconds,
        "transfer_mib_per_second": size as f64 / 1048576.0 / transfer_seconds,
        "during_gc": during, "released_logical_objects": released.removed.logical_objects,
        "released_pack_bytes": before_release - remaining,
        "after_release_gc": {"seconds": release_seconds, "phases": release_phases,
            "ledger": release_ledger},
        "pack_bytes_after_release": remaining, "correctness": "passed"})
    );
}

fn main() {
    let timings = gc_timing::Timings::install();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        // Reverse paired order between repetitions to reduce warmup/order bias.
        let reverse = std::env::var_os("CASITA_BENCH_TRANSFER_REVERSE").is_some();
        for size in [4096, 4 * 1024 * 1024] {
            for remote in [false, true] {
                for gc in [false, true] {
                    for snapshot in if reverse {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        scenario(size, remote, snapshot, gc, &timings).await;
                    }
                }
            }
        }
    });
}
