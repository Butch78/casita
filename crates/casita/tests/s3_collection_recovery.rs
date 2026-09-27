#![cfg(all(feature = "cli", feature = "s3"))]

#[path = "support/rustfs.rs"]
mod rustfs;

use casita::experimental::{
    MetadataStore, PinResource, Repository, Wal3MetadataStore, flush_repository_leases,
};
use std::{collections::BTreeSet, process::Command, time::Duration};

#[test]
fn s3_recovery_owner() {
    let Some(path) = std::env::var_os("CASITA_RECOVERY_TOKENS") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let repository = Repository::s3("casita-application-test", "recovery", "old-owner").await.unwrap();
        let mutation = repository.mutation_session().await.unwrap();
        let live = mutation.stage_blob(b"root retained after process recovery").await.unwrap();
        let live_key = live.record().key().clone();
        let orphan = mutation.stage_blob(b"orphan from interrupted collector").await.unwrap();
        let orphan_blob = orphan.record().payload();
        mutation.publish_rooted(vec![live, orphan], "live".parse().unwrap(), live_key.clone()).await.unwrap();
        drop(mutation);
        flush_repository_leases().await.unwrap();
        let state = repository.metadata();
        let operational = state.try_collection_lease().await.unwrap().unwrap();
        let token = state.repository_holds().await.unwrap()[0].token.clone();
        let ledger = state.pin_store().await.unwrap();
        let collector = ledger.begin_collection(ledger.inventory().await.unwrap().revision, None).await.unwrap().unwrap();
        let fence = ledger.begin_prune(ledger.inventory().await.unwrap().revision).await.unwrap().unwrap();
        ledger.claim_deletions_during_prune(ledger.inventory().await.unwrap().revision,
            BTreeSet::from([PinResource::Blob(orphan_blob)]), &collector, &fence).await.unwrap().unwrap();
        std::fs::write(path, serde_json::json!({"operational": token.as_str(), "collector": collector.to_string(), "live": live_key.to_string()}).to_string()).unwrap();
        // The owning process exits with durable claims and both ownership
        // records intact. No backend request is left running in this fixture.
        std::mem::forget(operational);
    });
}

#[test]
fn s3_recovery_runner() {
    let Some(path) = std::env::var_os("CASITA_RECOVERY_TOKENS") else {
        return;
    };
    let tokens: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(200),
                Repository::s3("casita-application-test", "recovery", "ordinary-reader")
            )
            .await
            .is_err()
        );
        let state =
            Wal3MetadataStore::open_s3("casita-application-test", "recovery/state", "recoverer")
                .await
                .unwrap();
        let operational = tokens["operational"].as_str().unwrap().parse().unwrap();
        assert!(
            state
                .release_abandoned_repository_hold(&operational)
                .await
                .unwrap()
        );
        let collector = tokens["collector"].as_str().unwrap().parse().unwrap();
        let outcome = Repository::recover_s3_collection(
            "casita-application-test",
            "recovery",
            "recoverer",
            &collector,
        )
        .await
        .unwrap();
        assert_eq!(outcome.removed.logical_objects, 1);
        flush_repository_leases().await.unwrap();
        let inventory = state.pin_store().await.unwrap().inventory().await.unwrap();
        assert!(inventory.collector.is_none());
        assert!(inventory.logical_prune.is_none());
        assert!(inventory.deletions.is_empty());
        assert!(state.repository_holds().await.unwrap().is_empty());
        let reopened = Repository::s3("casita-application-test", "recovery", "reader")
            .await
            .unwrap();
        let key = tokens["live"].as_str().unwrap().parse().unwrap();
        assert!(matches!(
            reopened.verify_closure(&key).await.unwrap(),
            casita::experimental::ClosureStatus::Complete { objects: 1 }
        ));
        drop(reopened);
        flush_repository_leases().await.unwrap();
    });
}

#[test]
fn separate_process_recovers_s3_catalog_through_an_abandoned_prune_fence() {
    let fixture = rustfs::Rustfs::start();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fixture.create_bucket());
    let directory = tempfile::tempdir().unwrap();
    for test in ["s3_recovery_owner", "s3_recovery_runner"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([test, "--exact", "--nocapture"])
            .env("CASITA_RECOVERY_TOKENS", directory.path().join("tokens"));
        fixture.configure(&mut command);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{test}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
