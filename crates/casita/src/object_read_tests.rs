//! Object-scoped read correctness and permanent local lifecycle benchmark.
use std::{collections::BTreeSet, path::Path, time::Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::metadata::{PinResource, PinScope};
use crate::{MetadataStore, ObjectKey, Repository, RootName};

trait ReadSeek: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin> ReadSeek for T {}

async fn local(path: &Path) -> Repository {
    Repository {
        inner: crate::repository::Repository::local_with_pack_options(
            path,
            crate::PackOptions {
                target_size: 128 * 1024,
                cache_capacity: 0,
            },
        )
        .await
        .unwrap()
        .into_builtin(),
    }
}

fn bytes(label: usize, size: usize) -> Vec<u8> {
    let mut output = vec![0; size];
    blake3::Hasher::new()
        .update(&(label as u64).to_le_bytes())
        .finalize_xof()
        .fill(&mut output);
    output
}

fn name(value: &str) -> RootName {
    value.try_into().unwrap()
}

async fn seed(repo: &Repository, size: usize, garbage: usize) -> (ObjectKey, Vec<ObjectKey>) {
    let held = repo
        .import(crate::import::BlobImport::new(
            bytes(0, size).as_slice(),
            name("held"),
        ))
        .await
        .unwrap();
    let mut dead = Vec::new();
    for index in 0..garbage {
        let root = name(&format!("garbage/{index}"));
        let key = repo
            .import(crate::import::BlobImport::new(
                bytes(index + 1, 32 * 1024).as_slice(),
                root.clone(),
            ))
            .await
            .unwrap();
        assert!(repo.remove_root(&root, &key).await.unwrap());
        dead.push(key);
    }
    if let Err(error) = repo.flush().await {
        assert_eq!(error.kind(), crate::ErrorKind::Busy);
    }
    (held, dead)
}

fn pack_bytes(path: &Path) -> u64 {
    fn walk(path: &Path) -> u64 {
        if !path.exists() {
            return 0;
        }
        std::fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    walk(&entry.path())
                } else {
                    metadata.len()
                }
            })
            .sum()
    }
    // Only immutable payload packs, excluding catalogs and metadata.
    walk(&path.join("blobs/packs"))
}

async fn case(size: usize, garbage: usize, mode: &str, warm: bool) -> Value {
    let snapshot = mode == "snapshot";
    let durable = mode == "durable-object";
    let dir = tempfile::tempdir().unwrap();
    let repo = local(dir.path()).await;
    let (held, dead) = seed(&repo, size, garbage).await;
    // Mutations also use process reader pins. Drop their cached reader owner
    // so the cold case measures first admission, then warm explicitly below.
    drop(repo);
    let repo = local(dir.path()).await;
    let ledger = repo.inner.metadata().pin_store().await.unwrap();
    if warm {
        if snapshot {
            drop(
                repo.retained_reader()
                    .await
                    .unwrap()
                    .open(&held)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        } else {
            let mut profile = crate::repository::ObjectReadProfile {
                durable_readers: durable,
                ..Default::default()
            };
            drop(
                repo.inner
                    .open_object_inner(&held, &mut profile)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        repo.flush().await.unwrap();
    }
    let durable_path = dir.path().join("casita.sqlite.online-pins");
    let durable_before = std::fs::read(&durable_path).unwrap();
    let before_revision = ledger.inventory().await.unwrap().revision;
    let start = Instant::now();
    let mut profile = crate::repository::ObjectReadProfile {
        durable_readers: durable,
        ..Default::default()
    };
    let mut reader: Box<dyn ReadSeek> = if snapshot {
        let session = repo.retained_reader().await.unwrap();
        profile.admission_nanos = start.elapsed().as_nanos() as u64;
        let resolve = Instant::now();
        let reader = session.open(&held).await.unwrap().unwrap();
        profile.resolve_nanos = resolve.elapsed().as_nanos() as u64;
        Box::new(reader)
    } else {
        Box::new(
            repo.inner
                .open_object_inner(&held, &mut profile)
                .await
                .unwrap()
                .unwrap()
                .1,
        )
    };
    let open_nanos = start.elapsed().as_nanos() as u64;
    let handoff = Instant::now();
    if snapshot {
        assert_eq!(
            repo.flush().await.unwrap_err().kind(),
            crate::ErrorKind::Busy
        );
    } else {
        repo.flush().await.unwrap();
    }
    let handoff_nanos = handoff.elapsed().as_nanos() as u64;
    let inventory = ledger.inventory().await.unwrap();
    assert_eq!(inventory.pins.len(), 1, "temporary snapshot pin must drain");
    let pin = inventory.pins.values().next().unwrap();
    if snapshot {
        assert!(matches!(pin.scope, PinScope::Snapshot { .. }));
    } else {
        assert_eq!(
            pin.scope,
            PinScope::Closures(BTreeSet::from([held.clone()]))
        );
        assert!(pin.catalog.is_none());
        assert!(
            pin.resources
                .iter()
                .all(|r| !matches!(r, PinResource::Catalog(_)))
        );
    }
    let open_ledger_updates = inventory.revision - before_revision;
    let open_durable_revision_changed =
        u8::from(std::fs::read(&durable_path).unwrap() != durable_before);
    if !durable && warm {
        assert_eq!(
            open_durable_revision_changed, 0,
            "warm opens must not rewrite the durable ledger"
        );
    } else {
        assert_eq!(open_durable_revision_changed, 1);
    }

    // Replace the root after admission, then collect from an independent handle
    // with the original repository facade dropped. The reader alone owns safety.
    assert!(repo.remove_root(&name("held"), &held).await.unwrap());
    if snapshot {
        assert_eq!(
            repo.flush().await.unwrap_err().kind(),
            crate::ErrorKind::Busy
        );
    } else {
        repo.flush().await.unwrap();
    }
    drop(repo);
    let collector = local(dir.path()).await;
    let before_bytes = pack_bytes(dir.path());
    assert!(before_bytes > 0, "fixture must have physical packs");
    let gc = Instant::now();
    let removed = collector.collect().await.unwrap().logical_objects;
    let gc_nanos = gc.elapsed().as_nanos() as u64;
    assert_eq!(removed, if snapshot { 0 } else { garbage });
    for key in &dead {
        assert_eq!(collector.object(key).await.unwrap().is_some(), snapshot);
    }
    assert!(collector.object(&held).await.unwrap().is_some());
    if snapshot {
        assert_eq!(
            collector.flush().await.unwrap_err().kind(),
            crate::ErrorKind::Busy
        );
    } else {
        collector.flush().await.unwrap();
    }
    let after_bytes = pack_bytes(dir.path());
    if !snapshot {
        assert!(
            after_bytes < before_bytes,
            "unrelated physical packs must be reclaimed"
        );
    }
    // Force another catalog replacement/compaction before the first lazy read.
    collector.vacuum().await.unwrap();
    let read = Instant::now();
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    let read_nanos = read.elapsed().as_nanos() as u64;
    assert_eq!(actual, bytes(0, size));
    let offset = size / 2;
    reader
        .seek(std::io::SeekFrom::Start(offset as u64))
        .await
        .unwrap();
    actual.clear();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, bytes(0, size)[offset..]);
    if snapshot {
        assert_eq!(
            collector.flush().await.unwrap_err().kind(),
            crate::ErrorKind::Busy
        );
    } else {
        collector.flush().await.unwrap();
    }
    let revision = ledger.inventory().await.unwrap().revision;
    let durable_before_release = std::fs::read(&durable_path).unwrap();
    let release = Instant::now();
    drop(reader);
    collector.flush().await.unwrap();
    let release_nanos = release.elapsed().as_nanos() as u64;
    let inventory = ledger.inventory().await.unwrap();
    assert!(inventory.pins.is_empty());
    let release_ledger_updates = inventory.revision - revision;
    let release_durable_revision_changed =
        u8::from(std::fs::read(&durable_path).unwrap() != durable_before_release);
    assert_eq!(release_durable_revision_changed, u8::from(durable));

    assert_eq!(
        collector.collect().await.unwrap().logical_objects,
        if snapshot { garbage + 1 } else { 1 }
    );
    assert!(collector.object(&held).await.unwrap().is_none());
    collector.flush().await.unwrap();
    assert!(ledger.inventory().await.unwrap().pins.is_empty());
    json!({"size": size, "garbage": garbage, "mode": mode, "reader_admission": if warm { "warm" } else { "cold" },
        "open_durable_revision_changed": open_durable_revision_changed, "release_durable_revision_changed": release_durable_revision_changed,
        "open_nanos": open_nanos, "admission_nanos": profile.admission_nanos,
        "resolve_nanos": profile.resolve_nanos, "handoff_nanos": handoff_nanos,
        "read_nanos": read_nanos, "release_nanos": release_nanos, "gc_nanos": gc_nanos,
        "open_ledger_updates": open_ledger_updates, "release_ledger_updates": release_ledger_updates,
        "gc_removed": removed, "pack_bytes_before": before_bytes, "pack_bytes_after": after_bytes,
        "correctness": "exact bytes and seek after GC, expected logical and physical collection, no leaked pins"})
}

#[tokio::test]
async fn object_read_collects_unrelated_garbage_and_survives_catalog_replacement() {
    for size in [0, 64, 1024 * 1024 + 17] {
        case(size, 4, "object", true).await;
    }
}

#[tokio::test]
async fn retained_reader_preserves_snapshot_wide_protection() {
    for warm in [false, true] {
        case(1024 * 1024 + 17, 4, "snapshot", warm).await;
    }
}

#[tokio::test]
async fn public_open_retains_only_selected_object_and_missing_open_releases_pins() {
    let repo = Repository::memory().unwrap();
    let (held, dead) = seed(&repo, 64, 2).await;
    let mut reader = repo.open(&held).await.unwrap().unwrap();
    repo.flush().await.unwrap();
    let replacement = repo
        .import(crate::import::BlobImport::new(
            bytes(99, 65).as_slice(),
            name("held"),
        ))
        .await
        .unwrap();
    assert_eq!(
        repo.root(&name("held")).await.unwrap(),
        Some(replacement.clone())
    );
    assert_eq!(repo.collect().await.unwrap().logical_objects, dead.len());
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, bytes(0, 64));
    drop(reader);
    repo.flush().await.unwrap();
    assert_eq!(repo.collect().await.unwrap().logical_objects, 1);
    assert!(repo.open(&held).await.unwrap().is_none());
    assert!(repo.object(&replacement).await.unwrap().is_some());
    repo.flush().await.unwrap();
    assert!(
        repo.inner
            .metadata()
            .pin_store()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
            .pins
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "permanent local read/GC lifecycle benchmark; run benchmark object-reads"]
async fn benchmark_object_reads() {
    let size = std::env::var("CASITA_READ_SIZE").unwrap().parse().unwrap();
    let garbage = std::env::var("CASITA_READ_GARBAGE")
        .unwrap()
        .parse()
        .unwrap();
    let mode = std::env::var("CASITA_READ_MODE").unwrap();
    assert!(matches!(
        mode.as_str(),
        "object" | "durable-object" | "snapshot"
    ));
    let admission = std::env::var("CASITA_READ_ADMISSION").unwrap_or_else(|_| "cold".into());
    assert!(matches!(admission.as_str(), "cold" | "warm"));
    println!(
        "object_read_sample {}",
        case(size, garbage, &mode, admission == "warm").await
    );
}

#[tokio::test]
async fn directory_reader_retains_transitive_children_but_not_unrelated_objects() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("child"), bytes(42, 64)).unwrap();
    let repo = local(&dir.path().join("repo")).await;
    let (_, garbage) = seed(&repo, 64, 2).await;
    repo.import(crate::import::FilesystemImport::new(&source, name("tree")))
        .await
        .unwrap();
    let tree = repo.root(&name("tree")).await.unwrap().unwrap();
    let reader = repo.open(&tree).await.unwrap().unwrap();
    let held = repo.root(&name("held")).await.unwrap().unwrap();
    assert!(repo.remove_root(&name("held"), &held).await.unwrap());
    assert!(repo.remove_root(&name("tree"), &tree).await.unwrap());
    repo.flush().await.unwrap();
    assert_eq!(
        repo.collect().await.unwrap().logical_objects,
        garbage.len() + 1
    );
    // Checkout traverses the child graph after both roots have disappeared.
    repo.checkout(&tree, dir.path().join("out")).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("out/child")).unwrap(),
        bytes(42, 64)
    );
    drop(reader);
    repo.flush().await.unwrap();
    assert!(repo.collect().await.unwrap().logical_objects >= 2);
}

#[tokio::test]
async fn cancelled_open_waiting_for_admission_leaks_no_pin() {
    let dir = tempfile::tempdir().unwrap();
    let repo = local(dir.path()).await;
    let (held, _) = seed(&repo, 64, 1).await;
    let ledger = repo.inner.metadata().pin_store().await.unwrap();
    let revision = ledger.inventory().await.unwrap().revision;
    let fence = ledger.begin_prune(revision).await.unwrap().unwrap();
    let opened = tokio::spawn({
        let repo = repo.clone();
        async move { repo.open(&held).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!opened.is_finished(), "prune fence must block admission");
    opened.abort();
    assert!(matches!(opened.await, Err(error) if error.is_cancelled()));
    ledger.finish_prune(&fence).await.unwrap();
    repo.flush().await.unwrap();
    assert!(ledger.inventory().await.unwrap().pins.is_empty());
}

#[test]
fn object_reader_child() {
    let Some(path) = std::env::var_os("CASITA_OBJECT_READER_CHILD") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let path = Path::new(&path);
        let repo = local(path).await;
        let held = repo.root(&name("held")).await.unwrap().unwrap();
        let _reader = repo.open(&held).await.unwrap().unwrap();
        repo.flush().await.unwrap();
        std::fs::write(path.join("reader-ready"), b"ready").unwrap();
        std::future::pending::<()>().await;
    });
}

#[tokio::test]
async fn killed_reader_releases_only_read_protection_after_kernel_owner_exit() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = local(dir.path()).await;
    let (held, dead) = seed(&repo, 1024 * 1024 + 17, 4).await;
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "object_read_tests::object_reader_child",
                "--exact",
                "--nocapture",
            ])
            .env("CASITA_OBJECT_READER_CHILD", dir.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    while !dir.path().join("reader-ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "reader child exited early"
        );
        assert!(
            Instant::now() < deadline,
            "reader child failed to become ready"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(repo.remove_root(&name("held"), &held).await.unwrap());
    repo.flush().await.unwrap();
    let ledger = repo.inner.metadata().pin_store().await.unwrap();
    let inventory = ledger.inventory().await.unwrap();
    assert_eq!(inventory.pins.len(), 1);
    assert_eq!(
        inventory.pins.values().next().unwrap().scope,
        PinScope::Closures(BTreeSet::from([held.clone()]))
    );
    // Collection must make progress even while the other process is alive.
    assert_eq!(repo.collect().await.unwrap().logical_objects, dead.len());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    // The collector independently probes the kernel owner lock. No timeout,
    // PID lookup, or explicit token release is needed for read-only protection.
    assert_eq!(repo.collect().await.unwrap().logical_objects, 1);
    assert!(repo.object(&held).await.unwrap().is_none());
    repo.flush().await.unwrap();
}

#[tokio::test]
async fn physical_plan_uses_protected_catalog_even_if_shared_catalog_changes() {
    use crate::BlobStore;
    let dir = tempfile::tempdir().unwrap();
    let repo = local(dir.path()).await;
    // An older reader must retain its external catalog object while later
    // publications and admission-time GC advance the shared handle.
    let empty = repo.inner.owned_retention_hold().await.unwrap();
    let empty_catalog = empty.snapshot().payload_catalog().unwrap().to_vec();
    let (held, _) = seed(&repo, 1024 * 1024 + 17, 1).await;
    let hold = repo.inner.owned_retention_hold().await.unwrap();
    let record = hold.object(&held).await.unwrap().unwrap();
    let pin = crate::metadata::DataPinLease::acquire(
        repo.inner.metadata().pin_store().await.unwrap(),
        crate::metadata::DataPin {
            scope: PinScope::Closures(BTreeSet::from([held.clone()])),
            catalog: None,
            resources: BTreeSet::from([PinResource::Blob(record.payload())]),
        },
    )
    .await
    .unwrap();
    // Simulate another reader synchronizing an older snapshot on the same
    // physical handle between logical admission and physical resolution.
    repo.inner
        .payloads()
        .publication()
        .synchronize_state_catalog(Some(&empty_catalog))
        .await
        .unwrap();
    drop(empty);
    let mut reader = repo
        .inner
        .payloads()
        .open_read_scoped(&record.payload(), pin, hold.snapshot().payload_catalog())
        .await
        .unwrap()
        .unwrap();
    drop(hold);
    repo.flush().await.unwrap();
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, bytes(0, 1024 * 1024 + 17));
    drop(reader);
    repo.flush().await.unwrap();
}

#[tokio::test]
async fn partially_consumed_planned_reader_survives_gc_and_releases_after_seek() {
    let dir = tempfile::tempdir().unwrap();
    let repo = local(dir.path()).await;
    let size = 24 * 1024 * 1024 + 17;
    let (held, _) = seed(&repo, size, 4).await;
    let mut reader = repo.open(&held).await.unwrap().unwrap();
    // Cross several chunk boundaries, starting the owned look-ahead pump.
    let mut prefix = vec![0; 1024 * 1024];
    reader.read_exact(&mut prefix).await.unwrap();
    let expected = bytes(0, size);
    assert_eq!(prefix, expected[..prefix.len()]);
    assert!(repo.remove_root(&name("held"), &held).await.unwrap());
    drop(repo);

    let collector = local(dir.path()).await;
    let ledger = collector.inner.metadata().pin_store().await.unwrap();
    collector.flush().await.unwrap();
    assert_eq!(ledger.inventory().await.unwrap().pins.len(), 1);
    assert_eq!(collector.collect().await.unwrap().logical_objects, 4);
    collector.vacuum().await.unwrap();
    let mut suffix = Vec::new();
    reader.read_to_end(&mut suffix).await.unwrap();
    assert_eq!(suffix, expected[prefix.len()..]);

    // Start another pump and abandon it with queued windows still owned.
    reader.seek(std::io::SeekFrom::Start(0)).await.unwrap();
    reader.read_exact(&mut prefix).await.unwrap();
    assert_eq!(prefix, expected[..prefix.len()]);
    drop(reader);
    collector.flush().await.unwrap();
    assert!(ledger.inventory().await.unwrap().pins.is_empty());
    assert_eq!(collector.collect().await.unwrap().logical_objects, 1);
    assert!(collector.object(&held).await.unwrap().is_none());
}

#[test]
fn reader_crash_worker() {
    let Some(path) = std::env::var_os("CASITA_READER_CRASH_REPOSITORY") else {
        return;
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let repo = local(Path::new(&path)).await;
            let key = repo.root(&name("held")).await.unwrap().unwrap();
            repo.flush().await.unwrap();
            let reader = repo.open(&key).await.unwrap().unwrap();
            drop(reader);
            repo.flush().await.unwrap();
            panic!("reader crash checkpoint was not reached");
        });
}

#[tokio::test]
async fn reader_crashes_at_owner_and_inventory_boundaries_leave_write_recovery_intact() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for phase in [
        "owner-locked",
        "owner-fenced",
        "reader-before-rename",
        "reader-after-rename",
        "reader-registered",
        "reader-protected",
        "reader-released",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let repo = local(dir.path()).await;
        let (held, dead) = seed(&repo, 1024 * 1024 + 17, 1).await;
        let ledger = repo.inner.metadata().pin_store().await.unwrap();
        let writer = ledger
            .register(crate::metadata::DataPin {
                scope: PinScope::Staging,
                catalog: None,
                resources: BTreeSet::from([PinResource::StorageObject("writer-recovery".into())]),
            })
            .await
            .unwrap()
            .unwrap();
        let signal = dir.path().join("crash-ready");
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "object_read_tests::reader_crash_worker",
                    "--exact",
                    "--nocapture",
                ])
                .env("CASITA_READER_CRASH_REPOSITORY", dir.path())
                .env("CASITA_READER_CRASH_PHASE", phase)
                .env("CASITA_READER_CRASH_SIGNAL", &signal)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        while !signal.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "{phase}: child exited early"
            );
            assert!(Instant::now() < deadline, "{phase}: checkpoint timed out");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(repo.remove_root(&name("held"), &held).await.unwrap());
        repo.flush().await.unwrap();
        assert_eq!(
            repo.collect().await.unwrap().logical_objects,
            dead.len() + 1,
            "{phase}"
        );
        let state = ledger.inventory().await.unwrap();
        assert_eq!(state.pins.len(), 1, "{phase}");
        assert!(
            state.pins.contains_key(&writer),
            "{phase}: writer recovery must survive"
        );
        ledger.release(&writer).await.unwrap();
        repo.flush().await.unwrap();
    }
}
