//! Process failures around durable protection, release and collection in a
//! multi-owner S3 repository. Recovery inspects and repairs the pin ledger,
//! which needs the experimental API; the owners themselves still use the
//! supported facade. Run with
//! `devenv shell cargo test --features s3,experimental --test s3_multi_owner_recovery`.
#![cfg(all(feature = "s3", feature = "experimental"))]

#[path = "support/multi_owner.rs"]
mod multi_owner;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use casita::experimental::{
    MetadataStore, PinResource, PinScope, PinStore, PinToken, Wal3MetadataStore,
    flush_repository_leases,
};
use casita::{ErrorKind, RetryDisposition, RootName};
use multi_owner::*;

const WORKER: &str = "s3_multi_owner_recovery_worker";

#[test]
fn s3_multi_owner_recovery_across_process_failures() {
    let fixture = rustfs::Rustfs::start();
    runtime().block_on(fixture.create_bucket());
    let root = tempfile::tempdir().unwrap();
    orphan_pin(&fixture, root.path());
    stale_release(&fixture, root.path());
    abandoned_collector(&fixture, root.path());
    busy_collector(&fixture, root.path());
}

#[test]
fn s3_multi_owner_recovery_worker() {
    let Some((context, scenario, phase)) = Context::from_env() else {
        return;
    };
    runtime().block_on(async {
        tokio::time::timeout(RENDEZVOUS_DEADLINE, async {
            match (scenario.as_str(), phase.as_str()) {
                ("orphan-pin", "publish") => orphan_pin_publish(&context).await,
                ("orphan-pin", "pinned-reader") => orphan_pin_reader(&context).await,
                ("orphan-pin", "collect-with-orphan") => orphan_pin_collect(&context).await,
                ("orphan-pin", "recover-pin") => orphan_pin_recover(&context).await,
                ("stale-release", "publish") => {
                    replacement_publish(&context, STALE_RELEASE_SEED).await
                }
                ("stale-release", "reader-a") => stale_release_reader(&context, "a").await,
                ("stale-release", "reader-b") => stale_release_reader(&context, "b").await,
                ("stale-release", "stale-cleanup") => stale_release_cleanup(&context).await,
                ("stale-release", "final") => stale_release_final(&context).await,
                ("abandoned-collector", "publish") => abandoned_collector_publish(&context).await,
                ("abandoned-collector", "abandon") => abandoned_collector_abandon(&context).await,
                ("abandoned-collector", "recover") => abandoned_collector_recover(&context).await,
                ("busy-collector", "publish") => busy_collector_publish(&context).await,
                ("busy-collector", "contend") => busy_collector_contend(&context).await,
                _ => panic!("unknown phase {scenario}/{phase}"),
            }
        })
        .await
        .expect("recovery phase must finish before the rendezvous deadline");
    });
}

/// The scenario's durable pin ledger, opened without taking any ownership.
async fn ledger(context: &Context) -> Arc<dyn PinStore> {
    Wal3MetadataStore::open_s3(
        rustfs::BUCKET,
        format!("{}/state", context.prefix),
        "inspector",
    )
    .await
    .unwrap()
    .pin_store()
    .await
    .unwrap()
}

/// Tokens of snapshot pins that still protect data. Released pins may linger
/// as retired liveness history until a collector finishes; those are not
/// protection and are excluded.
async fn active_snapshot_pins(context: &Context) -> Vec<String> {
    let inventory = ledger(context).await.inventory().await.unwrap();
    inventory
        .pins
        .iter()
        .filter(|(token, pin)| {
            matches!(pin.scope, PinScope::Snapshot { .. }) && !inventory.retired.contains(token)
        })
        .map(|(token, _)| token.to_string())
        .collect()
}

// Scenario F: a reader process dies while its durable pin is armed. Nothing
// expires the pin, so collection keeps the content until an operator releases
// that exact token; then the next pass reclaims it and nothing else.

fn orphan_pin(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "orphan-pin");
    scenario.run("publish");
    let mut reader = scenario.worker("pinned-reader").spawn();
    worker::await_file(
        &scenario.file("pinned-ready"),
        &mut [&mut reader],
        RENDEZVOUS_DEADLINE,
    );
    reader.kill();
    scenario.run("collect-with-orphan");
    scenario.run("recover-pin");
}

async fn orphan_pin_publish(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let key_a = owner_a
        .import(casita::import::BlobImport::new(
            &payload(51)[..],
            name("owner-a/current"),
        ))
        .await
        .unwrap();
    let owner_b = context.remote("owner-b").await;
    let key_b = owner_b
        .import(casita::import::BlobImport::new(
            &payload(52)[..],
            name("owner-b/current"),
        ))
        .await
        .unwrap();
    write_key(&context.work, "key-a", &key_a);
    write_key(&context.work, "key-b", &key_b);
    owner_a.flush().await.unwrap();
    owner_b.flush().await.unwrap();
}

async fn orphan_pin_reader(context: &Context) {
    let key = read_key(&context.work, "key-a");
    let repository = context.remote("reader-a").await;
    let session = repository.retained_reader().await.unwrap();
    let reader = session.open(&key).await.unwrap().unwrap();
    let pins = active_snapshot_pins(context).await;
    assert_eq!(pins.len(), 1, "{pins:?}");
    std::fs::write(context.file("token"), &pins[0]).unwrap();
    worker::signal(&context.file("pinned-ready"));
    // The parent kills this process while the pin is armed.
    std::future::pending::<()>().await;
    drop(reader);
    drop(session);
    drop(repository);
}

async fn orphan_pin_collect(context: &Context) {
    let key_a = read_key(&context.work, "key-a");
    let token = std::fs::read_to_string(context.file("token")).unwrap();
    let owner_b = context.remote("owner-b").await;
    assert!(
        owner_b
            .remove_root(&name("owner-a/current"), &key_a)
            .await
            .unwrap()
    );
    owner_b.flush().await.unwrap();
    // The dead reader's pin is durable: no timeout expires it, so the
    // unrooted content stays readable and collection reclaims nothing.
    assert_eq!(owner_b.collect().await.unwrap().logical_objects, 0);
    assert_eq!(read_verified(&owner_b, &key_a).await, payload(51));
    assert!(active_snapshot_pins(context).await.contains(&token));
    // A stale pin delays reclamation; it does not block integrity audits.
    assert!(owner_b.fsck().await.unwrap().is_healthy());
    owner_b.flush().await.unwrap();
}

async fn orphan_pin_recover(context: &Context) {
    let key_a = read_key(&context.work, "key-a");
    let key_b = read_key(&context.work, "key-b");
    let token = read_value::<PinToken>(&context.work, "token");
    let ledger = ledger(context).await;
    // Releasing the exact token is the recovery step. A duplicate release
    // is harmless: it names a token that no longer protects anything.
    ledger.release(&token).await.unwrap();
    ledger.release(&token).await.unwrap();
    flush_repository_leases().await.unwrap();
    assert!(active_snapshot_pins(context).await.is_empty());
    let collector = context.remote("collector").await;
    assert_eq!(collector.collect().await.unwrap().logical_objects, 1);
    assert!(collector.open(&key_a).await.unwrap().is_none());
    assert_eq!(read_verified(&collector, &key_b).await, payload(52));
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
    let inventory = ledger.inventory().await.unwrap();
    assert!(
        inventory
            .pins
            .values()
            .all(|pin| !matches!(pin.scope, PinScope::Snapshot { .. })),
        "{inventory:?}"
    );
}

// Scenario G: owner A's pin is released after owner B armed its own on the
// same content. A delayed duplicate of A's release, replayed by cleanup,
// must not remove B's replacement protection.

const STALE_RELEASE_SEED: u64 = 71;

fn stale_release(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "stale-release");
    run_replacement(&scenario, "stale-cleanup");
}

/// The shared replacement reader, additionally recording this process's own
/// pin token so cleanup can replay owner A's release by name.
async fn stale_release_reader(context: &Context, owner: &str) {
    replacement_reader(context, owner, STALE_RELEASE_SEED, async || {
        // This process's pin is the active snapshot pin no earlier reader recorded.
        let known: Vec<String> = ["token-a", "token-b"]
            .iter()
            .filter_map(|file| std::fs::read_to_string(context.file(file)).ok())
            .collect();
        let mine: Vec<String> = active_snapshot_pins(context)
            .await
            .into_iter()
            .filter(|token| !known.contains(token))
            .collect();
        assert_eq!(mine.len(), 1, "{mine:?} known {known:?}");
        write_value(&context.work, &format!("token-{owner}"), &mine[0]);
    })
    .await;
}

async fn stale_release_cleanup(context: &Context) {
    let key = read_key(&context.work, "key");
    let token_a: PinToken = read_value(&context.work, "token-a");
    let token_b = std::fs::read_to_string(context.file("token-b")).unwrap();
    let ledger = ledger(context).await;
    // Owner A released and drained before exiting. A delayed duplicate of
    // that release names only A's token and must leave B's pin untouched.
    ledger.release(&token_a).await.unwrap();
    flush_repository_leases().await.unwrap();
    assert_eq!(active_snapshot_pins(context).await, vec![token_b.clone()]);
    let collector = context.remote("collector").await;
    assert!(
        collector
            .remove_root(&name("owner-a/current"), &key)
            .await
            .unwrap()
    );
    collector.flush().await.unwrap();
    assert_eq!(collector.collect().await.unwrap().logical_objects, 0);
    assert!(collector.object(&key).await.unwrap().is_some());
    assert_eq!(active_snapshot_pins(context).await, vec![token_b]);
    collector.flush().await.unwrap();
}

async fn stale_release_final(context: &Context) {
    replacement_final(context).await;
    assert!(active_snapshot_pins(context).await.is_empty());
}

// Scenario H: a collector dies holding the operational hold, collector
// ownership, a prune fence and a deletion claim while two owners' content
// shares chunks. Recovery with the exact tokens reclaims only the orphan.

fn abandoned_collector(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "abandoned-collector");
    scenario.run("publish");
    scenario.run("abandon");
    scenario.run("recover");
}

async fn abandoned_collector_publish(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let key_a = owner_a
        .import(casita::import::BlobImport::new(
            &payload(61)[..],
            name("owner-a/current"),
        ))
        .await
        .unwrap();
    let owner_b = context.remote("owner-b").await;
    let key_b = owner_b
        .overwrite_blob(
            &key_a,
            OVERWRITE_OFFSET,
            &small(7)[..OVERWRITE_LEN],
            name("owner-b/current"),
        )
        .await
        .unwrap();
    write_key(&context.work, "key-a", &key_a);
    write_key(&context.work, "key-b", &key_b);
    owner_a.flush().await.unwrap();
    owner_b.flush().await.unwrap();
}

async fn abandoned_collector_abandon(context: &Context) {
    let repository =
        casita::experimental::Repository::s3(rustfs::BUCKET, &context.prefix, "old-collector")
            .await
            .unwrap();
    let mutation = repository.mutation_session().await.unwrap();
    let orphan = mutation
        .stage_blob(b"orphan from an interrupted collector")
        .await
        .unwrap();
    let orphan_blob = orphan.record().payload();
    mutation.publish_unrooted(vec![orphan]).await.unwrap();
    drop(mutation);
    flush_repository_leases().await.unwrap();
    let state = repository.metadata();
    let operational = state.try_collection_lease().await.unwrap().unwrap();
    let holds = state.repository_holds().await.unwrap();
    assert_eq!(holds.len(), 1);
    assert!(holds[0].exclusive);
    let ledger = state.pin_store().await.unwrap();
    let revision = ledger.inventory().await.unwrap().revision;
    let collector = ledger
        .begin_collection(revision, None)
        .await
        .unwrap()
        .unwrap();
    let revision = ledger.inventory().await.unwrap().revision;
    let fence = ledger.begin_prune(revision).await.unwrap().unwrap();
    let revision = ledger.inventory().await.unwrap().revision;
    ledger
        .claim_deletions_during_prune(
            revision,
            BTreeSet::from([PinResource::Blob(orphan_blob)]),
            &collector,
            &fence,
        )
        .await
        .unwrap()
        .unwrap();
    std::fs::write(context.file("hold"), holds[0].token.as_str()).unwrap();
    std::fs::write(context.file("collector"), collector.to_string()).unwrap();
    // Exit with the operational hold, collector ownership, prune fence and
    // deletion claim all intact, as a crashed collector would leave them.
    std::mem::forget(operational);
}

async fn abandoned_collector_recover(context: &Context) {
    let key_a = read_key(&context.work, "key-a");
    let key_b = read_key(&context.work, "key-b");
    let hold: RootName = std::fs::read_to_string(context.file("hold"))
        .unwrap()
        .parse()
        .unwrap();
    let collector = read_value::<PinToken>(&context.work, "collector");
    let state = Wal3MetadataStore::open_s3(
        rustfs::BUCKET,
        format!("{}/state", context.prefix),
        "recoverer",
    )
    .await
    .unwrap();
    // The operator has established that the old collector stopped. Release
    // its exact operational hold, then resume its pass with its exact token.
    assert!(
        state
            .release_abandoned_repository_hold(&hold)
            .await
            .unwrap()
    );
    let outcome = casita::experimental::Repository::recover_s3_collection(
        rustfs::BUCKET,
        &context.prefix,
        "recoverer",
        &collector,
    )
    .await
    .unwrap();
    // Recovery may retain extra data; it must never delete protected content.
    assert!(outcome.removed.logical_objects <= 1);
    flush_repository_leases().await.unwrap();
    let inventory = state.pin_store().await.unwrap().inventory().await.unwrap();
    assert!(inventory.collector.is_none());
    assert!(inventory.logical_prune.is_none());
    assert!(inventory.deletions.is_empty());
    assert!(state.repository_holds().await.unwrap().is_empty());
    let repository = context.remote("owner-b").await;
    assert_eq!(read_verified(&repository, &key_a).await, payload(61));
    assert_eq!(read_verified(&repository, &key_b).await, overwritten(61, 7));
    // Whatever recovery retained, the next ordinary pass reclaims exactly
    // the orphan and nothing of either owner.
    assert_eq!(
        repository.collect().await.unwrap().logical_objects + outcome.removed.logical_objects,
        1
    );
    assert!(repository.fsck().await.unwrap().is_clean());
    repository.flush().await.unwrap();
}

// Scenario I: `try_collect` reports contention instead of waiting, whether
// another runner holds the operational collector lease or another collector
// owns the pin ledger. The blocking `collect` would wait on the former.

fn busy_collector(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "busy-collector");
    scenario.run("publish");
    scenario.run("contend");
}

async fn busy_collector_publish(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let key = owner_a
        .import(casita::import::BlobImport::new(
            &small(5)[..],
            name("owner-a/scratch"),
        ))
        .await
        .unwrap();
    assert!(
        owner_a
            .remove_root(&name("owner-a/scratch"), &key)
            .await
            .unwrap()
    );
    write_key(&context.work, "key", &key);
    owner_a.flush().await.unwrap();
}

fn assert_busy(error: casita::Error) {
    assert_eq!(error.kind(), ErrorKind::Busy, "{error:?}");
    assert_eq!(error.retry_disposition(), RetryDisposition::Retry);
}

async fn busy_collector_contend(context: &Context) {
    let key = read_key(&context.work, "key");
    let collector = context.remote("collector").await;
    // Another runner holds the operational collector lease.
    let other =
        casita::experimental::Repository::s3(rustfs::BUCKET, &context.prefix, "other-runner")
            .await
            .unwrap();
    let lease = other
        .metadata()
        .try_collection_lease()
        .await
        .unwrap()
        .unwrap();
    assert_busy(collector.try_collect().await.unwrap_err());
    drop(lease);
    flush_repository_leases().await.unwrap();
    // Another collector owns the pin ledger.
    let ledger = ledger(context).await;
    let revision = ledger.inventory().await.unwrap().revision;
    let token = ledger
        .begin_collection(revision, None)
        .await
        .unwrap()
        .unwrap();
    assert_busy(collector.try_collect().await.unwrap_err());
    assert!(collector.object(&key).await.unwrap().is_some());
    ledger.finish_collection(&token).await.unwrap();
    // Each Busy pass above acquired and then asynchronously released the
    // operational lease; drain those before requiring a pass to succeed.
    flush_repository_leases().await.unwrap();
    assert_eq!(collector.try_collect().await.unwrap().logical_objects, 1);
    assert!(collector.object(&key).await.unwrap().is_none());
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
    drop(other);
    flush_repository_leases().await.unwrap();
}
