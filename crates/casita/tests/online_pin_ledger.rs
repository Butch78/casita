//! Protocol tests only: repository GC must not be enabled concurrently until
//! storage writes, logical marking, and physical reclamation use this ledger.
#![cfg(all(feature = "experimental", feature = "s3"))]

#[path = "support/rustfs.rs"]
mod rustfs;

use casita::experimental::{
    DataPin, FilePinStore, ObjectPinStore, PinResource, PinScope, PinStore,
};
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn resources(path: &str) -> BTreeSet<PinResource> {
    BTreeSet::from([PinResource::StorageObject(path.into())])
}

fn pin(path: &str) -> DataPin {
    DataPin {
        scope: PinScope::Staging,
        catalog: None,
        resources: resources(path),
    }
}

fn remote(endpoint: &str) -> ObjectPinStore {
    let store = AmazonS3Builder::new()
        .with_bucket_name(rustfs::BUCKET)
        .with_region("us-east-1")
        .with_endpoint(endpoint)
        .with_allow_http(true)
        .with_access_key_id("minio")
        .with_secret_access_key("minio123")
        .with_conditional_put(S3ConditionalPut::ETagMatch)
        .build()
        .unwrap();
    ObjectPinStore::new(Arc::new(store), "online-pins/inventory".into())
}

#[test]
fn pin_owner_process() {
    let Some(location) = std::env::var_os("CASITA_TEST_PIN_LEDGER") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store: Box<dyn PinStore> =
            if let Some(endpoint) = std::env::var_os("CASITA_TEST_PIN_ENDPOINT") {
                Box::new(remote(endpoint.to_str().unwrap()))
            } else {
                Box::new(FilePinStore::new(location))
            };
        store.begin_collection(0, None).await.unwrap().unwrap();
        let completed = store
            .register(pin("completed-writer-pack"))
            .await
            .unwrap()
            .unwrap();
        store.release(&completed).await.unwrap();
        store.register(pin("reader-pack")).await.unwrap().unwrap();
        let revision = store.inventory().await.unwrap().revision;
        store
            .claim_deletions(revision, resources("deleting-pack"))
            .await
            .unwrap()
            .unwrap();
        // Exit with unresolved protection. The next process must observe it.
    });
}

fn owner(location: &std::path::Path, fixture: Option<&rustfs::Rustfs>) {
    let output_dir = tempfile::tempdir().unwrap();
    let stdout = output_dir.path().join("stdout");
    let stderr = output_dir.path().join("stderr");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["pin_owner_process", "--exact", "--nocapture"])
        .env("CASITA_TEST_PIN_LEDGER", location)
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    if let Some(fixture) = fixture {
        fixture.configure(&mut command);
        command.env("CASITA_TEST_PIN_ENDPOINT", fixture.endpoint());
    } else {
        command.env_remove("CASITA_TEST_PIN_ENDPOINT");
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "{}\n{}",
                std::fs::read_to_string(&stdout).unwrap(),
                std::fs::read_to_string(&stderr).unwrap()
            );
            return;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "pin owner timed out: {}",
                std::fs::read_to_string(&stderr).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

async fn recover(store: &dyn PinStore) {
    let inventory = store.inventory().await.unwrap();
    assert_eq!(inventory.pins.len(), 2);
    assert_eq!(inventory.retired.len(), 1);
    assert_eq!(inventory.deletions.len(), 1);
    assert!(
        store
            .claim_deletions(inventory.revision, resources("completed-writer-pack"))
            .await
            .unwrap()
            .is_none()
    );
    let collector = store
        .begin_collection(inventory.revision, inventory.collector.clone())
        .await
        .unwrap()
        .unwrap();
    store
        .finish_collection(inventory.collector.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(store.inventory().await.unwrap().retired, inventory.retired);
    assert!(
        store
            .register(pin("deleting-pack"))
            .await
            .unwrap()
            .is_none()
    );
    let live = store
        .register(pin("unrelated-write"))
        .await
        .unwrap()
        .unwrap();
    let revision = store.inventory().await.unwrap().revision;
    assert!(
        store
            .claim_deletions(revision, resources("reader-pack"))
            .await
            .unwrap()
            .is_none()
    );
    let garbage = store
        .claim_deletions(revision, resources("unrelated-garbage"))
        .await
        .unwrap()
        .unwrap();
    store.finish_deletions(&garbage).await.unwrap();
    let deleting = inventory.deletions.keys().next().unwrap();
    // The child has exited and this parent owns recovery. The same persisted
    // transition must preserve claims on both local files and S3 conditional writes.
    let before_recovery = store.inventory().await.unwrap();
    let fence = store
        .begin_prune_recovering(before_recovery.revision, BTreeSet::from([deleting.clone()]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.inventory().await.unwrap().deletions,
        before_recovery.deletions
    );
    assert!(
        store
            .register(pin("deleting-pack"))
            .await
            .unwrap()
            .is_none()
    );
    store.finish_prune(&fence).await.unwrap();
    let reader = store
        .register(DataPin {
            scope: PinScope::Snapshot {
                generation: u64::MAX,
            },
            catalog: None,
            resources: BTreeSet::new(),
        })
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .inventory()
            .await
            .unwrap()
            .deletions
            .contains_key(deleting)
    );
    store.release(&reader).await.unwrap();
    assert!(
        store
            .register(pin("deleting-pack"))
            .await
            .unwrap()
            .is_none()
    );
    store.finish_deletions(deleting).await.unwrap();
    let replacement = store.register(pin("deleting-pack")).await.unwrap().unwrap();
    store.finish_deletions(deleting).await.unwrap();
    assert!(
        store
            .inventory()
            .await
            .unwrap()
            .pins
            .contains_key(&replacement)
    );
    store.release(&live).await.unwrap();
    store.finish_collection(&collector).await.unwrap();
    let finished = store.inventory().await.unwrap();
    assert!(finished.collector.is_none());
    assert!(finished.retired.is_empty());
    assert!(finished.pins.contains_key(&replacement));
    assert!(
        inventory
            .retired
            .iter()
            .all(|token| !finished.pins.contains_key(token))
    );
}

#[test]
fn local_pins_and_deletion_claims_survive_process_exit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pins");
    owner(&path, None);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recover(&FilePinStore::new(path)));
}

#[test]
fn s3_pins_survive_process_exit_and_concurrent_clients_preserve_every_owner() {
    let fixture = rustfs::Rustfs::start();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(fixture.create_bucket());
    owner(std::path::Path::new("remote"), Some(&fixture));
    runtime.block_on(async {
        let collector = remote(&fixture.endpoint());
        recover(&collector).await;
        let baseline = collector.inventory().await.unwrap().pins.len();
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..8 {
            let writer = remote(&fixture.endpoint());
            tasks.spawn(async move {
                writer
                    .register(pin(&format!("writer-{index}")))
                    .await
                    .unwrap()
                    .unwrap()
            });
        }
        let mut tokens = Vec::new();
        while let Some(result) = tasks.join_next().await {
            tokens.push(result.unwrap());
        }
        let inventory = collector.inventory().await.unwrap();
        assert_eq!(inventory.pins.len(), baseline + tokens.len());
        assert!(
            tokens
                .iter()
                .all(|token| inventory.pins.contains_key(token))
        );
    });
}
