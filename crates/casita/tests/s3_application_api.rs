//! Public application workflows against a real S3-compatible server.
//! Run with `devenv shell cargo test --features s3 --test s3_application_api`.
#![cfg(feature = "s3")]

#[path = "support/rustfs.rs"]
mod rustfs;

use std::io::SeekFrom;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use casita::{
    ErrorKind, MetadataChange, MetadataCheck, MetadataCommitResult, MetadataKey, ObjectKey,
    Repository, RootName,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const PHASE: &str = "CASITA_S3_API_PHASE";
const WORK: &str = "CASITA_S3_API_WORK";
const PREFIX: &str = "application";

fn name(value: &str) -> RootName {
    value.try_into().unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// Deterministic bytes span several chunks without collapsing under compression.
fn payload(mut state: u64) -> Vec<u8> {
    (0..3 * 1024 * 1024 + 127)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn worker(fixture: &rustfs::Rustfs, work: &Path, phase: &str) {
    let log_path = work.join(format!("{phase}.log"));
    let log = std::fs::File::create(&log_path).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["s3_application_worker", "--exact", "--nocapture"])
        .env(PHASE, phase)
        .env(WORK, work)
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    fixture.configure(&mut command);
    if phase == "download" {
        // Exercise the service-specific endpoint as well as the shared AWS
        // endpoint used for publication and collection.
        command
            .env_remove("AWS_ENDPOINT_URL")
            .env("AWS_ENDPOINT_URL_S3", fixture.endpoint());
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "{phase} failed: {}",
                std::fs::read_to_string(log_path).unwrap()
            );
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{phase} timed out: {}",
                std::fs::read_to_string(log_path).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn s3_roundtrip_retention_and_shutdown_through_the_application_api() {
    let fixture = rustfs::Rustfs::start();
    runtime().block_on(fixture.create_bucket());
    let work = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(work.path().join("source/nested/empty")).unwrap();
    std::fs::write(work.path().join("source/nested/data"), payload(42)).unwrap();
    std::fs::write(work.path().join("source/hello"), b"public S3 API\n").unwrap();
    // Each phase has fresh clients and a fresh runtime, so cached state and
    // process-local coordination cannot hide failed publication or cleanup.
    for phase in ["upload", "retention", "download"] {
        worker(&fixture, work.path(), phase);
    }
}

#[test]
fn s3_application_worker() {
    let Ok(phase) = std::env::var(PHASE) else {
        return;
    };
    let work = std::path::PathBuf::from(std::env::var_os(WORK).unwrap());
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(60), async {
            match phase.as_str() {
                "upload" => upload(&work).await,
                "retention" => retention().await,
                "download" => download(&work).await,
                _ => panic!("unknown test phase {phase}"),
            }
        })
        .await
        .expect("S3 application phase must finish within sixty seconds");
    });
}

async fn remote(writer: &str) -> Repository {
    Repository::s3(rustfs::BUCKET, PREFIX, writer)
        .await
        .unwrap()
}

async fn upload(work: &Path) {
    let source = Repository::local(work.join("local-source")).await.unwrap();
    let key = source
        .import(casita::import::FilesystemImport::new(
            work.join("source"),
            name("project"),
        ))
        .await
        .unwrap();
    let destination = remote("uploader").await;
    assert_eq!(
        destination
            .import(casita::import::CopyImport::new(
                &source,
                name("project"),
                name("remote/project")
            ))
            .await
            .unwrap(),
        key
    );
    assert_eq!(
        destination.root(&name("remote/project")).await.unwrap(),
        Some(key.clone())
    );
    let temporary = destination
        .import(casita::import::BlobImport::new(
            &payload(93)[..],
            name("temporary/blob"),
        ))
        .await
        .unwrap();
    checked_root_commit(&destination, &key, &temporary).await;
    assert!(
        destination
            .compare_and_set_root(name("promoted/project"), None, key.clone())
            .await
            .unwrap()
    );
    let observer = remote("promotion-observer").await;
    assert!(
        !observer
            .compare_and_set_root(name("promoted/project"), None, key.clone())
            .await
            .unwrap()
    );
    assert!(
        observer
            .compare_and_set_root(name("promoted/project"), Some(&key), temporary.clone())
            .await
            .unwrap()
    );
    assert!(
        !destination
            .compare_and_set_root(name("promoted/project"), Some(&key), key.clone())
            .await
            .unwrap()
    );
    assert_eq!(
        destination.root(&name("promoted/project")).await.unwrap(),
        Some(temporary.clone())
    );
    assert!(
        observer
            .remove_root(&name("promoted/project"), &temporary)
            .await
            .unwrap()
    );
    observer.flush().await.unwrap();
    std::fs::write(work.join("expected-root"), key.to_string()).unwrap();
    destination.flush().await.unwrap();
    source.flush().await.unwrap();
}

// Root wrappers keep working on S3 through the shared commit entrypoint.
// Multi-root conflicts must keep caller order, not the backend's name order.
async fn checked_root_commit(repository: &Repository, first: &ObjectKey, second: &ObjectKey) {
    let z = name("commit/z");
    let a = name("commit/a");
    repository.set_root(z.clone(), first.clone()).await.unwrap();
    repository.set_root(a.clone(), first.clone()).await.unwrap();
    let snapshot = repository.metadata_reader().await.unwrap();
    let original_roots = snapshot.roots().await.unwrap();
    let original_revision = snapshot.revision();
    assert_eq!(original_roots, repository.roots().await.unwrap());
    let changes = vec![
        MetadataChange::SetRoot {
            name: z.clone(),
            target: second.clone(),
        },
        MetadataChange::RemoveRoot { name: a.clone() },
    ];
    assert_eq!(
        repository
            .commit(
                vec![
                    MetadataCheck::Root {
                        name: z.clone(),
                        expected: None
                    },
                    MetadataCheck::Root {
                        name: a.clone(),
                        expected: None
                    },
                ],
                changes.clone()
            )
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 },
    );
    assert_eq!(
        repository
            .commit(
                vec![
                    MetadataCheck::Root {
                        name: z.clone(),
                        expected: Some(first.clone())
                    },
                    MetadataCheck::Root {
                        name: z.clone(),
                        expected: None
                    },
                ],
                changes.clone()
            )
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 1 },
    );
    assert_eq!(repository.root(&z).await.unwrap().as_ref(), Some(first));
    assert_eq!(repository.root(&a).await.unwrap().as_ref(), Some(first));

    let record = MetadataKey::new("obrador.v1".parse().unwrap(), "paths/test");
    assert_eq!(
        snapshot
            .get(std::slice::from_ref(&record))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        snapshot.scan(&record, None, 1).await.unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    let mut mixed = changes.clone();
    mixed.push(MetadataChange::Set {
        key: record.clone(),
        value: "descriptor".into(),
    });
    assert_eq!(
        repository
            .commit(Vec::new(), mixed)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        repository
            .commit(
                vec![MetadataCheck::Record {
                    key: record,
                    expected: None
                }],
                changes.clone()
            )
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(repository.root(&z).await.unwrap().as_ref(), Some(first));
    assert_eq!(repository.root(&a).await.unwrap().as_ref(), Some(first));

    assert!(matches!(
        repository
            .commit(
                vec![
                    MetadataCheck::Root {
                        name: z.clone(),
                        expected: Some(first.clone())
                    },
                    MetadataCheck::Root {
                        name: a.clone(),
                        expected: Some(first.clone())
                    },
                ],
                changes
            )
            .await
            .unwrap(),
        MetadataCommitResult::Committed { .. }
    ));
    assert_eq!(repository.root(&z).await.unwrap().as_ref(), Some(second));
    assert_eq!(repository.root(&a).await.unwrap(), None);
    assert_eq!(snapshot.revision(), original_revision);
    assert_eq!(snapshot.roots().await.unwrap(), original_roots);
    assert_eq!(snapshot.root(&a).await.unwrap().as_ref(), Some(first));
    assert_eq!(snapshot.root(&z).await.unwrap().as_ref(), Some(first));
    assert!(repository.remove_root(&z, second).await.unwrap());
    assert!(!repository.remove_root(&z, second).await.unwrap());
}

async fn retention() {
    let repository = remote("reader").await;
    let collector = remote("collector").await;
    let metadata = repository.metadata_reader().await.unwrap();
    let metadata_clone = metadata.clone();
    drop(metadata);
    let session = repository.retained_reader().await.unwrap();
    let key = session
        .root(&name("temporary/blob"))
        .await
        .unwrap()
        .unwrap();
    let bytes = payload(93);
    drop(repository);
    assert!(
        collector
            .remove_root(&name("temporary/blob"), &key)
            .await
            .unwrap()
    );
    // Flush drains dropped leases while preserving the live reader's lease.
    collector.flush().await.unwrap();
    let collected = tokio::time::timeout(Duration::from_secs(15), collector.collect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(collected.logical_objects, 0);
    // Resolve and open after another handle removed the root and ran GC.
    assert_eq!(
        session.root(&name("temporary/blob")).await.unwrap(),
        Some(key.clone())
    );
    let mut reader = session.open(&key).await.unwrap().unwrap();
    assert_eq!(reader.record().payload_size(), bytes.len() as u64);
    drop(session);
    collector.flush().await.unwrap();
    assert_eq!(collector.collect().await.unwrap().logical_objects, 0);
    let offset = 1024 * 1024 + 17;
    reader.seek(SeekFrom::Start(offset as u64)).await.unwrap();
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).await.unwrap();
    assert_eq!(tail, bytes[offset..]);
    drop(reader);
    collector.flush().await.unwrap();
    let removed = tokio::time::timeout(Duration::from_secs(15), collector.collect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(removed.logical_objects, 1);
    assert!(collector.object(&key).await.unwrap().is_none());
    assert!(collector.open(&key).await.unwrap().is_none());
    collector.vacuum().await.unwrap();
    assert_eq!(
        metadata_clone.root(&name("temporary/blob")).await.unwrap(),
        Some(key.clone())
    );
    assert_eq!(
        metadata_clone.object(&key).await.unwrap().unwrap().key(),
        &key
    );
    drop(metadata_clone);
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
    scoped_retention().await;
}

// A single-object reader must allow unrelated preexisting garbage to be
// collected, unlike the explicit retained snapshot exercised above.
async fn scoped_retention() {
    let repository = remote("scoped-reader").await;
    let collector = remote("scoped-collector").await;
    let bytes = payload(57);
    let key = repository
        .import(casita::import::BlobImport::new(
            &bytes[..],
            name("reader/scoped"),
        ))
        .await
        .unwrap();
    let garbage = repository
        .import(casita::import::BlobImport::new(
            &b"unrelated reader garbage"[..],
            name("reader/unrelated"),
        ))
        .await
        .unwrap();
    assert!(
        repository
            .remove_root(&name("reader/unrelated"), &garbage)
            .await
            .unwrap()
    );
    repository.flush().await.unwrap();
    let mut reader = repository.open_verified(&key).await.unwrap().unwrap();
    drop(repository);
    assert!(
        collector
            .remove_root(&name("reader/scoped"), &key)
            .await
            .unwrap()
    );
    collector.flush().await.unwrap();
    let collected = tokio::time::timeout(Duration::from_secs(15), collector.collect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(collected.logical_objects, 1);
    assert!(collector.object(&garbage).await.unwrap().is_none());
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, bytes);
    drop(reader);
    collector.flush().await.unwrap();
    assert_eq!(collector.collect().await.unwrap().logical_objects, 1);
    assert!(collector.open(&key).await.unwrap().is_none());
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
}

async fn download(work: &Path) {
    let source = remote("downloader").await;
    let key = source.root(&name("remote/project")).await.unwrap().unwrap();
    assert_eq!(
        key.to_string(),
        std::fs::read_to_string(work.join("expected-root")).unwrap()
    );
    assert!(
        source
            .root(&name("temporary/blob"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(source.roots().await.unwrap().len(), 1);
    let mirror = Repository::local(work.join("local-mirror")).await.unwrap();
    assert_eq!(
        mirror
            .import(casita::import::CopyImport::new(
                &source,
                name("remote/project"),
                name("restored")
            ))
            .await
            .unwrap(),
        key
    );
    mirror.checkout(&key, work.join("checkout")).await.unwrap();
    for file in ["hello", "nested/data"] {
        assert_eq!(
            std::fs::read(work.join("checkout").join(file)).unwrap(),
            std::fs::read(work.join("source").join(file)).unwrap()
        );
    }
    assert!(work.join("checkout/nested/empty").is_dir());
    assert!(source.fsck().await.unwrap().is_clean());
    assert!(mirror.fsck().await.unwrap().is_clean());
    source.flush().await.unwrap();
    mirror.flush().await.unwrap();
}
