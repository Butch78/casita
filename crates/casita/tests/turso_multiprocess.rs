#![cfg(feature = "experimental")]

//! A real process boundary for the local Turso database. The in-crate tests
//! cover multiple handles inside one process; this proves that the experimental
//! shared-WAL path, rather than only Turso's in-process connection sharing,
//! keeps two casita processes writing one live repository.
//!
//! The parent commits before the child starts and again after it exits, so the
//! assertions cover both directions of cross-process visibility without any
//! timing: the child observes a root published before it opened the database,
//! and the parent's pre-existing connections observe the child's commit.

#![cfg(feature = "native")]

use std::process::Command;

use casita::experimental::{ClosureStatus, MetadataStore as _, Repository, RootChange, RootName};
use tokio::io::AsyncReadExt as _;

const CONCURRENT_WRITERS: usize = 8;
const CONCURRENT_GENERATIONS: usize = 16;

const CHILD_TEST: &str = "child_writes_the_live_repository";
const REPOSITORY_ENV: &str = "CASITA_TURSO_MULTIPROCESS_REPOSITORY";

const PARENT_BYTES: &[u8] = b"written before the child opened the database";
const CHILD_BYTES: &[u8] = b"written in a separate process";

fn parent_root() -> RootName {
    RootName::try_from("multiprocess/parent").unwrap()
}

fn child_root() -> RootName {
    RootName::try_from("multiprocess/child").unwrap()
}

fn later_root() -> RootName {
    RootName::try_from("multiprocess/parent-after-child").unwrap()
}

/// Re-execute this test binary as a separate casita process pointed at the
/// same repository, running only [`child_writes_the_live_repository`].
fn run_child(repository: &std::path::Path) {
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(CHILD_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(REPOSITORY_ENV, repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Turso child failed: stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The child half of the boundary. Without the environment variable this is an
/// ordinary no-op test; the parent re-executes this binary with it set.
#[test]
fn child_writes_the_live_repository() {
    let Ok(root) = std::env::var(REPOSITORY_ENV) else {
        return;
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let repository = Repository::local(root).await.unwrap();

        let snapshot = repository.metadata().snapshot().await.unwrap();
        let inherited = snapshot
            .root(&parent_root())
            .await
            .unwrap()
            .expect("the parent's committed root is visible in another process");
        drop(snapshot);

        let mutation = repository.mutation_session().await.unwrap();
        let staged = mutation.stage_blob(CHILD_BYTES).await.unwrap();
        let key = staged.record().key().clone();
        assert_ne!(key, inherited);
        mutation
            .publish_rooted(vec![staged], child_root(), key)
            .await
            .unwrap();
        drop(mutation);
        drop(repository);
        casita::experimental::flush_repository_leases()
            .await
            .unwrap();
    });
}

#[tokio::test]
async fn a_second_process_opens_and_writes_the_live_database() {
    let dir = tempfile::tempdir().unwrap();
    let parent = Repository::local(dir.path()).await.unwrap();

    let parent_key = {
        let mutation = parent.mutation_session().await.unwrap();
        let staged = mutation.stage_blob(PARENT_BYTES).await.unwrap();
        let key = staged.record().key().clone();
        mutation
            .publish_rooted(vec![staged], parent_root(), key.clone())
            .await
            .unwrap();
        key
    };

    run_child(dir.path());

    // Connections opened before the child existed see its committed root, the
    // record behind it, and the payload it wrote.
    let snapshot = parent.metadata().snapshot().await.unwrap();
    let child_key = snapshot
        .root(&child_root())
        .await
        .unwrap()
        .expect("the child's root is visible in the parent process");
    assert_eq!(
        snapshot.root(&parent_root()).await.unwrap(),
        Some(parent_key)
    );
    let child_record = snapshot
        .object(&child_key)
        .await
        .unwrap()
        .expect("the child's record is visible in the parent process");
    drop(snapshot);
    // Repository admission synchronizes the payload catalog to the same
    // committed state, as it does for S3. Raw payload handles do not refresh
    // themselves from an independently managed metadata snapshot.
    let hold = parent.retention_hold().await.unwrap();
    assert_eq!(hold.object(&child_key).await.unwrap(), Some(child_record));
    let (_, mut reader) = hold.open_payload(&child_key).await.unwrap().unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, CHILD_BYTES);
    drop(reader);
    drop(hold);
    assert!(matches!(
        parent.verify_closure(&child_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));

    // The parent's writer connection still commits after a foreign process
    // advanced the WAL under it.
    {
        let mutation = parent.mutation_session().await.unwrap();
        mutation
            .publish(
                Vec::new(),
                vec![RootChange::Set {
                    name: later_root(),
                    target: child_key.clone(),
                }],
            )
            .await
            .unwrap();
    }

    drop(parent);

    // Nothing holds the database now. A fresh process must reacquire the
    // shared lifetime lock, recover whatever the exiting process left in the
    // WAL, and commit again.
    run_child(dir.path());

    let reopened = Repository::local(dir.path()).await.unwrap();
    let snapshot = reopened.metadata().snapshot().await.unwrap();
    assert_eq!(
        snapshot.root(&child_root()).await.unwrap(),
        Some(child_key.clone())
    );
    assert_eq!(snapshot.root(&later_root()).await.unwrap(), Some(child_key));
    assert!(snapshot.root(&parent_root()).await.unwrap().is_some());
    drop(snapshot);
    assert!(reopened.fsck().await.unwrap().is_healthy());
}

#[test]
fn concurrent_writer_child() {
    let Ok(path) = std::env::var("CASITA_CONCURRENT_WRITER_PATH") else {
        return;
    };
    let index = std::env::var("CASITA_CONCURRENT_WRITER_INDEX").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let repository = Repository::local(&path).await.unwrap();
        std::fs::write(
            std::path::Path::new(&path).join(format!("ready-{index}")),
            b"",
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !std::path::Path::new(&path).join("start-writers").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for generation in 0..CONCURRENT_GENERATIONS {
            let mutation = repository.mutation_session().await.unwrap();
            let bytes = format!("writer {index} generation {generation}");
            let staged = mutation.stage_blob(bytes.as_bytes()).await.unwrap();
            let key = staged.record().key().clone();
            mutation
                .publish_rooted(
                    vec![staged],
                    RootName::try_from(format!("concurrent/{index}/{generation}")).unwrap(),
                    key,
                )
                .await
                .unwrap();
        }
        drop(repository);
        casita::experimental::flush_repository_leases()
            .await
            .unwrap();
    });
}

#[test]
fn independent_processes_can_publish_concurrently() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let repository = runtime
        .block_on(Repository::local(directory.path()))
        .unwrap();
    let mut children = Vec::new();
    for index in 0..CONCURRENT_WRITERS {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["concurrent_writer_child", "--exact", "--nocapture"])
                .env("CASITA_CONCURRENT_WRITER_PATH", directory.path())
                .env("CASITA_CONCURRENT_WRITER_INDEX", index.to_string())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !(0..CONCURRENT_WRITERS)
        .all(|index| directory.path().join(format!("ready-{index}")).exists())
    {
        if std::time::Instant::now() >= deadline {
            for child in &mut children {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("concurrent writers did not become ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::fs::write(directory.path().join("start-writers"), b"").unwrap();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    runtime.block_on(async {
        let snapshot = repository.metadata().snapshot().await.unwrap();
        for index in 0..CONCURRENT_WRITERS {
            for generation in 0..CONCURRENT_GENERATIONS {
                let name = RootName::try_from(format!("concurrent/{index}/{generation}")).unwrap();
                let key = snapshot.root(&name).await.unwrap().unwrap();
                let status = repository.verify_closure(&key).await.unwrap();
                assert!(
                    matches!(status, ClosureStatus::Complete { .. }),
                    "{name}: {status:?}"
                );
            }
        }
    });
}
