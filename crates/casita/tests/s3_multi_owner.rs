//! Independent owners sharing immutable content in one S3 repository through
//! the supported application API only. Each owner holds opaque root names;
//! writers, readers and the collector run in separate processes.
//! Run with `devenv shell cargo test --features s3 --test s3_multi_owner`.
#![cfg(feature = "s3")]

#[path = "support/multi_owner.rs"]
mod multi_owner;

use std::io::SeekFrom;
use std::path::Path;

use casita::{
    ErrorKind, MetadataChange, MetadataCheck, MetadataCommitResult, ObjectKey, Repository,
    RetryDisposition, RootName,
};
use multi_owner::*;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const WORKER: &str = "s3_multi_owner_worker";
#[test]
fn s3_multi_owner_composition_through_the_application_api() {
    let fixture = rustfs::Rustfs::start();
    runtime().block_on(fixture.create_bucket());
    let root = tempfile::tempdir().unwrap();
    // Every phase has fresh clients and a fresh runtime, so process-local
    // state cannot stand in for durable publication, protection or release.
    shared_closure(&fixture, root.path());
    shared_chunks(&fixture, root.path());
    conflicts(&fixture, root.path());
    replacement_protection(&fixture, root.path());
    publication_versus_collection(&fixture, root.path());
}

#[test]
fn s3_multi_owner_worker() {
    let Some((context, scenario, phase)) = Context::from_env() else {
        return;
    };
    runtime().block_on(async {
        tokio::time::timeout(RENDEZVOUS_DEADLINE, async {
            match (scenario.as_str(), phase.as_str()) {
                ("same-closure", "publish-a") => same_closure_publish_a(&context).await,
                ("same-closure", "mirror-b") => same_closure_mirror_b(&context).await,
                ("same-closure", "reader") => same_closure_reader(&context).await,
                ("same-closure", "remove-a") => same_closure_remove_a(&context).await,
                ("same-closure", "remove-b-collect") => {
                    same_closure_remove_b_collect(&context).await
                }
                ("same-closure", "collect-after-release") => {
                    same_closure_collect_after_release(&context).await
                }
                ("shared-chunks", "publish-shared") => shared_chunks_publish(&context).await,
                ("shared-chunks", "remove-a-verify-b") => {
                    shared_chunks_remove_a_verify_b(&context).await
                }
                ("shared-chunks", "remove-b-collect") => {
                    shared_chunks_remove_b_collect(&context).await
                }
                ("conflicts", "conflicts") => conflicts_phase(&context).await,
                ("replacement", "publish") => replacement_publish(&context, REPLACEMENT_SEED).await,
                ("replacement", "reader-a") => {
                    replacement_reader(&context, "a", REPLACEMENT_SEED, async || {}).await
                }
                ("replacement", "reader-b") => {
                    replacement_reader(&context, "b", REPLACEMENT_SEED, async || {}).await
                }
                ("replacement", "remove-collect") => replacement_remove_collect(&context).await,
                ("replacement", "final") => replacement_final(&context).await,
                ("race", "seed") => race_seed(&context).await,
                ("race", "publisher") => race_publisher(&context).await,
                ("race", "collector") => race_collector(&context).await,
                ("race", "settle") => race_settle(&context).await,
                _ => panic!("unknown phase {scenario}/{phase}"),
            }
        })
        .await
        .expect("multi-owner phase must finish before the rendezvous deadline");
    });
}

// Scenario A: two owners name the same closure. Removing either owner keeps
// the other's verified content; a retained reader in a third process keeps
// the content after both names are gone; releasing it allows collection.

fn shared_closure(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "same-closure");
    scenario.run("publish-a");
    scenario.run("mirror-b");
    let mut reader = scenario.worker("reader").spawn();
    worker::await_file(
        &scenario.file("reader-ready"),
        &mut [&mut reader],
        RENDEZVOUS_DEADLINE,
    );
    scenario.run("remove-a");
    scenario.run("remove-b-collect");
    worker::signal(&scenario.file("reader-check"));
    worker::await_file(
        &scenario.file("reader-done"),
        &mut [&mut reader],
        RENDEZVOUS_DEADLINE,
    );
    worker::signal(&scenario.file("reader-release"));
    reader.wait(RENDEZVOUS_DEADLINE);
    scenario.run("collect-after-release");
}

async fn same_closure_publish_a(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let key = owner_a
        .import(casita::import::BlobImport::new(
            &payload(11)[..],
            name("owner-a/current"),
        ))
        .await
        .unwrap();
    assert_eq!(
        owner_a.preview_collection().await.unwrap().logical_objects,
        0
    );
    write_key(&context.work, "key", &key);
    owner_a.flush().await.unwrap();
}

async fn same_closure_mirror_b(context: &Context) {
    let key = read_key(&context.work, "key");
    let owner_b = context.remote("owner-b").await;
    assert_eq!(
        owner_b.root(&name("owner-a/current")).await.unwrap(),
        Some(key.clone())
    );
    assert!(
        owner_b
            .compare_and_set_root(name("owner-b/mirror"), None, key.clone())
            .await
            .unwrap()
    );
    // A duplicate delivery of the same claim is rejected without harm.
    assert!(
        !owner_b
            .compare_and_set_root(name("owner-b/mirror"), None, key.clone())
            .await
            .unwrap()
    );
    assert_eq!(owner_b.roots().await.unwrap().len(), 2);
    owner_b.flush().await.unwrap();
}

async fn same_closure_reader(context: &Context) {
    let key = read_key(&context.work, "key");
    let repository = context.remote("reader-b").await;
    let session = repository.retained_reader().await.unwrap();
    assert_eq!(
        session.root(&name("owner-b/mirror")).await.unwrap(),
        Some(key.clone())
    );
    let mut reader = session.open(&key).await.unwrap().unwrap();
    // Protection follows the session, not the handle that created it, and
    // survives dropping the original session while a clone remains.
    let clone = session.clone();
    drop(session);
    let session = clone;
    drop(repository);
    worker::signal(&context.file("reader-ready"));
    wait_for(&context.file("reader-check")).await;
    // Both names are gone and a collection ran; the snapshot is unchanged.
    assert_eq!(
        session.root(&name("owner-a/current")).await.unwrap(),
        Some(key.clone())
    );
    let expected = payload(11);
    let mut verified = session.open_verified(&key).await.unwrap().unwrap();
    let mut actual = Vec::new();
    verified.read_to_end(&mut actual).await.unwrap();
    assert_eq!(actual, expected);
    let offset = 1024 * 1024 + 17;
    reader.seek(SeekFrom::Start(offset)).await.unwrap();
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).await.unwrap();
    assert_eq!(tail, expected[offset as usize..]);
    worker::signal(&context.file("reader-done"));
    wait_for(&context.file("reader-release")).await;
    drop(verified);
    drop(reader);
    drop(session);
    // Drain the release before exit so the next phase observes it durably.
    context
        .remote("reader-b-flush")
        .await
        .flush()
        .await
        .unwrap();
}

async fn same_closure_remove_a(context: &Context) {
    let key = read_key(&context.work, "key");
    let owner_a = context.remote("owner-a").await;
    assert!(
        owner_a
            .remove_root(&name("owner-a/current"), &key)
            .await
            .unwrap()
    );
    assert!(
        !owner_a
            .remove_root(&name("owner-a/current"), &key)
            .await
            .unwrap()
    );
    // Owner B's name still retains the closure.
    assert_eq!(
        owner_a.preview_collection().await.unwrap().logical_objects,
        0
    );
    assert_eq!(read_verified(&owner_a, &key).await, payload(11));
    assert!(owner_a.fsck().await.unwrap().is_clean());
    owner_a.flush().await.unwrap();
}

async fn same_closure_remove_b_collect(context: &Context) {
    let key = read_key(&context.work, "key");
    let collector = context.remote("collector").await;
    assert!(
        collector
            .remove_root(&name("owner-b/mirror"), &key)
            .await
            .unwrap()
    );
    collector.flush().await.unwrap();
    // Only the retained reader in another process protects the closure now.
    assert_eq!(collector.collect().await.unwrap().logical_objects, 0);
    assert!(collector.object(&key).await.unwrap().is_some());
    collector.flush().await.unwrap();
}

async fn same_closure_collect_after_release(context: &Context) {
    let key = read_key(&context.work, "key");
    let collector = context.remote("collector").await;
    let removed = collector.collect().await.unwrap();
    assert_eq!(removed.logical_objects, 1);
    assert_eq!(removed.payload_blobs, 1);
    assert!(removed.chunks >= 1, "{removed:?}");
    assert!(collector.open(&key).await.unwrap().is_none());
    collector.vacuum().await.unwrap();
    assert!(collector.fsck().await.unwrap().is_clean());
    assert!(collector.roots().await.unwrap().is_empty());
    collector.flush().await.unwrap();
}

// Scenario B: distinct closures that share chunks and objects. Sharing is
// measured through collection previews rather than assumed from chunking.

fn shared_chunks(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "shared-chunks");
    scenario.run("publish-shared");
    scenario.run("remove-a-verify-b");
    scenario.run("remove-b-collect");
}

async fn preview_without(
    repository: &Repository,
    removed: &[(&str, &ObjectKey)],
) -> casita::CollectionReport {
    for (root, key) in removed {
        assert!(repository.remove_root(&name(root), key).await.unwrap());
    }
    let preview = repository.preview_collection().await.unwrap();
    for (root, key) in removed {
        // Records outlive their names until collection, so a name can be
        // restored without republishing.
        assert!(
            repository
                .compare_and_set_root(name(root), None, (*key).clone())
                .await
                .unwrap()
        );
    }
    preview
}

async fn shared_chunks_publish(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let owner_b = context.remote("owner-b").await;
    let key_a = owner_a
        .import(casita::import::BlobImport::new(
            &payload(21)[..],
            name("owner-a/current"),
        ))
        .await
        .unwrap();
    let key_b = owner_b
        .overwrite_blob(
            &key_a,
            OVERWRITE_OFFSET,
            &small(99)[..OVERWRITE_LEN],
            name("owner-b/current"),
        )
        .await
        .unwrap();
    assert_ne!(key_a, key_b);
    for (directory, private) in [("dir-a", 1), ("dir-b", 2)] {
        let path = context.file(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("shared"), payload(31)).unwrap();
        std::fs::write(path.join("private"), payload(private)).unwrap();
    }
    let tree_a = owner_a
        .import(casita::import::FilesystemImport::new(
            context.file("dir-a"),
            name("owner-a/tree"),
        ))
        .await
        .unwrap();
    let tree_b = owner_b
        .import(casita::import::FilesystemImport::new(
            context.file("dir-b"),
            name("owner-b/tree"),
        ))
        .await
        .unwrap();
    assert_ne!(tree_a, tree_b);
    assert_eq!(
        owner_a.preview_collection().await.unwrap().logical_objects,
        0
    );

    let only_b = preview_without(&owner_a, &[("owner-b/current", &key_b)]).await;
    assert_eq!((only_b.logical_objects, only_b.payload_blobs), (1, 1));
    let only_a = preview_without(&owner_a, &[("owner-a/current", &key_a)]).await;
    assert_eq!((only_a.logical_objects, only_a.payload_blobs), (1, 1));
    let both = preview_without(
        &owner_a,
        &[("owner-a/current", &key_a), ("owner-b/current", &key_b)],
    )
    .await;
    assert_eq!((both.logical_objects, both.payload_blobs), (2, 2));
    // Each blob has private chunks, and together they have fewer chunks than
    // the sum of two independent copies: the untouched chunks are shared.
    assert!(
        only_a.chunks >= 1 && only_b.chunks >= 1,
        "{only_a:?} {only_b:?}"
    );
    assert!(
        both.chunks > only_a.chunks + only_b.chunks,
        "{both:?} {only_a:?} {only_b:?}"
    );
    // Removing one tree frees its directory and private file only; the
    // shared file object and payload stay reachable through the other tree.
    let tree_only_a = preview_without(&owner_a, &[("owner-a/tree", &tree_a)]).await;
    assert_eq!(
        (tree_only_a.logical_objects, tree_only_a.payload_blobs),
        (2, 1)
    );
    let tree_only_b = preview_without(&owner_a, &[("owner-b/tree", &tree_b)]).await;
    assert_eq!(
        (tree_only_b.logical_objects, tree_only_b.payload_blobs),
        (2, 1)
    );
    assert_eq!(
        owner_a.preview_collection().await.unwrap().logical_objects,
        0
    );

    write_key(&context.work, "key-a", &key_a);
    write_key(&context.work, "key-b", &key_b);
    write_key(&context.work, "tree-a", &tree_a);
    write_key(&context.work, "tree-b", &tree_b);
    write_count(&context.work, "chunks-only-a", only_a.chunks);
    write_count(&context.work, "chunks-tree-a", tree_only_a.chunks);
    owner_a.flush().await.unwrap();
    owner_b.flush().await.unwrap();
}

async fn shared_chunks_remove_a_verify_b(context: &Context) {
    let key_a = read_key(&context.work, "key-a");
    let key_b = read_key(&context.work, "key-b");
    let tree_a = read_key(&context.work, "tree-a");
    let tree_b = read_key(&context.work, "tree-b");
    let owner_b = context.remote("owner-b").await;
    assert!(
        owner_b
            .remove_root(&name("owner-a/current"), &key_a)
            .await
            .unwrap()
    );
    assert!(
        owner_b
            .remove_root(&name("owner-a/tree"), &tree_a)
            .await
            .unwrap()
    );
    owner_b.flush().await.unwrap();
    let removed = owner_b.collect().await.unwrap();
    // Owner A's blob, directory and private file go; shared chunks and the
    // shared file object stay because owner B still reaches them.
    assert_eq!((removed.logical_objects, removed.payload_blobs), (3, 2));
    assert_eq!(
        removed.chunks,
        read_count(&context.work, "chunks-only-a") + read_count(&context.work, "chunks-tree-a")
    );
    assert!(owner_b.object(&key_a).await.unwrap().is_none());
    assert_eq!(read_verified(&owner_b, &key_b).await, overwritten(21, 99));
    let checkout = context.file("checkout-b");
    owner_b.checkout(&tree_b, &checkout).await.unwrap();
    assert_eq!(std::fs::read(checkout.join("shared")).unwrap(), payload(31));
    assert_eq!(std::fs::read(checkout.join("private")).unwrap(), payload(2));
    assert!(owner_b.fsck().await.unwrap().is_clean());
    owner_b.flush().await.unwrap();
}

async fn shared_chunks_remove_b_collect(context: &Context) {
    let key_b = read_key(&context.work, "key-b");
    let tree_b = read_key(&context.work, "tree-b");
    let collector = context.remote("collector").await;
    assert!(
        collector
            .remove_root(&name("owner-b/current"), &key_b)
            .await
            .unwrap()
    );
    assert!(
        collector
            .remove_root(&name("owner-b/tree"), &tree_b)
            .await
            .unwrap()
    );
    collector.flush().await.unwrap();
    let removed = collector.collect().await.unwrap();
    // Owner B's blob, directory and private file, plus the shared file
    // object that no tree references any more.
    assert_eq!((removed.logical_objects, removed.payload_blobs), (4, 3));
    assert!(collector.open(&key_b).await.unwrap().is_none());
    collector.vacuum().await.unwrap();
    assert!(collector.fsck().await.unwrap().is_clean());
    assert!(collector.roots().await.unwrap().is_empty());
    collector.flush().await.unwrap();
}

// Scenario C: conditional root operations between two owners. Checks compare
// current values, not history, so a delayed create-if-absent can succeed
// after the name was removed. A fence root closes that gap using only the
// atomic multi-root commit.

fn conflicts(fixture: &rustfs::Rustfs, root: &Path) {
    Scenario::new(fixture, root, WORKER, "conflicts").run("conflicts");
}

fn check(name: &RootName, expected: Option<&ObjectKey>) -> MetadataCheck {
    MetadataCheck::Root {
        name: name.clone(),
        expected: expected.cloned(),
    }
}

fn set(name: &RootName, target: &ObjectKey) -> MetadataChange {
    MetadataChange::SetRoot {
        name: name.clone(),
        target: target.clone(),
    }
}

async fn conflicts_phase(context: &Context) {
    let owner_a = context.remote("owner-a").await;
    let owner_b = context.remote("owner-b").await;
    let shared = name("shared/current");
    let fence = name("owner-a/fence");
    let mut candidates = Vec::new();
    for seed in 1..=3 {
        let content = small(seed);
        candidates.push(
            owner_a
                .import(casita::import::BlobImport::new(
                    &content[..],
                    name(&format!("owner-a/staging/{seed}")),
                ))
                .await
                .unwrap(),
        );
    }
    let (k1, k2, k3) = (&candidates[0], &candidates[1], &candidates[2]);
    let mut epochs = Vec::new();
    for epoch in 0..4 {
        let content = format!("epoch {epoch}");
        epochs.push(
            owner_a
                .import(casita::import::BlobImport::new(
                    content.as_bytes(),
                    name(&format!("owner-a/epochs/{epoch}")),
                ))
                .await
                .unwrap(),
        );
    }
    let (e0, e1, e2, e3) = (&epochs[0], &epochs[1], &epochs[2], &epochs[3]);

    // Create, repoint and remove conflicts between two owners.
    assert!(
        owner_a
            .compare_and_set_root(shared.clone(), None, k1.clone())
            .await
            .unwrap()
    );
    assert!(
        !owner_b
            .compare_and_set_root(shared.clone(), None, k1.clone())
            .await
            .unwrap()
    );
    assert!(
        owner_b
            .compare_and_set_root(shared.clone(), Some(k1), k2.clone())
            .await
            .unwrap()
    );
    assert!(!owner_a.remove_root(&shared, k1).await.unwrap());
    assert!(
        !owner_a
            .compare_and_set_root(shared.clone(), Some(k1), k3.clone())
            .await
            .unwrap()
    );
    assert_eq!(owner_a.root(&shared).await.unwrap().as_ref(), Some(k2));
    assert_eq!(
        owner_a
            .commit(vec![check(&shared, Some(k1))], vec![set(&shared, k3)])
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 }
    );

    // The limitation, made explicit: after the name is removed, a delayed
    // duplicate of the original create-if-absent succeeds and resurrects
    // the first target. Nothing in the root record remembers the removal.
    assert!(owner_b.remove_root(&shared, k2).await.unwrap());
    assert!(
        owner_a
            .compare_and_set_root(shared.clone(), None, k1.clone())
            .await
            .unwrap()
    );
    assert_eq!(owner_a.root(&shared).await.unwrap().as_ref(), Some(k1));
    assert!(owner_a.remove_root(&shared, k1).await.unwrap());

    // A fenced transition: every change to the shared name also advances an
    // owner-scoped fence root and checks the fence value it observed. A
    // delayed duplicate then fails on the fence even when the name's current
    // value would have satisfied the original check.
    owner_a.set_root(fence.clone(), e0.clone()).await.unwrap();
    let create_checks = vec![check(&fence, Some(e0)), check(&shared, None)];
    let create_changes = vec![set(&shared, k1), set(&fence, e1)];
    assert!(matches!(
        owner_a
            .commit(create_checks.clone(), create_changes.clone())
            .await
            .unwrap(),
        MetadataCommitResult::Committed { .. }
    ));
    assert_eq!(
        owner_a
            .commit(create_checks.clone(), create_changes.clone())
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 }
    );
    assert_eq!(owner_a.root(&shared).await.unwrap().as_ref(), Some(k1));
    assert_eq!(owner_a.root(&fence).await.unwrap().as_ref(), Some(e1));
    let remove_checks = vec![check(&fence, Some(e1)), check(&shared, Some(k1))];
    let remove_changes = vec![
        MetadataChange::RemoveRoot {
            name: shared.clone(),
        },
        set(&fence, e2),
    ];
    assert!(matches!(
        owner_a
            .commit(remove_checks.clone(), remove_changes.clone())
            .await
            .unwrap(),
        MetadataCommitResult::Committed { .. }
    ));
    // The delayed create now conflicts and the name stays absent.
    assert_eq!(
        owner_a.commit(create_checks, create_changes).await.unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 }
    );
    assert_eq!(owner_a.root(&shared).await.unwrap(), None);
    assert_eq!(
        owner_a.commit(remove_checks, remove_changes).await.unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 }
    );
    // A competitor that observed an older fence value cannot advance it.
    assert_eq!(
        owner_b
            .commit(
                vec![check(&fence, Some(e0))],
                vec![set(&shared, k3), set(&fence, e3)]
            )
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 0 }
    );
    // Conflicts report the first failing check in caller order.
    assert_eq!(
        owner_b
            .commit(
                vec![check(&shared, None), check(&fence, Some(e0))],
                vec![set(&shared, k3), set(&fence, e3)]
            )
            .await
            .unwrap(),
        MetadataCommitResult::Conflict { check_index: 1 }
    );
    assert_eq!(owner_a.root(&shared).await.unwrap(), None);
    assert_eq!(owner_a.root(&fence).await.unwrap().as_ref(), Some(e2));

    // Duplicate deliveries of unconditional operations are harmless.
    owner_a.set_root(shared.clone(), k2.clone()).await.unwrap();
    owner_a.set_root(shared.clone(), k2.clone()).await.unwrap();
    assert_eq!(owner_a.root(&shared).await.unwrap().as_ref(), Some(k2));
    assert!(owner_a.remove_root(&shared, k2).await.unwrap());
    assert!(!owner_a.remove_root(&shared, k2).await.unwrap());
    owner_a.flush().await.unwrap();
    owner_a.flush().await.unwrap();
    assert!(owner_a.fsck().await.unwrap().is_clean());
    owner_b.flush().await.unwrap();
}

// Scenario E: owner A releases its retained reader only after owner B armed
// its own on the same content. B's protection must hold on its own.

const REPLACEMENT_SEED: u64 = 41;

fn replacement_protection(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "replacement");
    run_replacement(&scenario, "remove-collect");
}

async fn replacement_remove_collect(context: &Context) {
    let key = read_key(&context.work, "key");
    let collector = context.remote("collector").await;
    assert!(
        collector
            .remove_root(&name("owner-a/current"), &key)
            .await
            .unwrap()
    );
    collector.flush().await.unwrap();
    // Owner A's release has settled; only owner B's reader protects the data.
    assert_eq!(collector.collect().await.unwrap().logical_objects, 0);
    assert!(collector.object(&key).await.unwrap().is_some());
    collector.flush().await.unwrap();
}

// Scenario D: one owner publishes and repoints repeatedly while the
// collector runs back to back in another process. Overlap is guaranteed by
// rendezvous files, and the invariant is a count: every superseded object is
// reclaimed exactly once, across the racing collector and a final pass.

const RACE_ITERATIONS: u64 = 12;

fn publication_versus_collection(fixture: &rustfs::Rustfs, root: &Path) {
    let scenario = Scenario::new(fixture, root, WORKER, "race");
    scenario.run("seed");
    let publisher = scenario.worker("publisher").spawn();
    let collector = scenario.worker("collector").spawn();
    publisher.wait(RENDEZVOUS_DEADLINE);
    collector.wait(RENDEZVOUS_DEADLINE);
    scenario.run("settle");
}

/// Retry while the error is one the application is expected to retry,
/// recording each kind seen. Bounded by attempts, never by time.
async fn retry<T, F, Fut>(seen: &mut Vec<ErrorKind>, mut operation: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, casita::Error>>,
{
    for _ in 0..10_000 {
        match operation().await {
            Ok(value) => return value,
            Err(error) if matches!(error.retry_disposition(), RetryDisposition::Retry) => {
                seen.push(error.kind());
            }
            Err(error) => panic!("operation failed without a retry disposition: {error:?}"),
        }
    }
    panic!("operation never succeeded: {seen:?}");
}

async fn race_seed(context: &Context) {
    let owner_b = context.remote("owner-b").await;
    let key = owner_b
        .import(casita::import::BlobImport::new(
            &small(0)[..],
            name("owner-b/current"),
        ))
        .await
        .unwrap();
    write_key(&context.work, "seed", &key);
    owner_b.flush().await.unwrap();
}

async fn race_publisher(context: &Context) {
    let owner_b = context.remote("owner-b").await;
    let current = name("owner-b/current");
    let mut previous = read_key(&context.work, "seed");
    let mut seen = Vec::new();
    for iteration in 1..=RACE_ITERATIONS {
        let item = name(&format!("owner-b/items/{iteration}"));
        let content = small(iteration);
        let key = retry(&mut seen, || {
            owner_b.import(casita::import::BlobImport::new(&content[..], item.clone()))
        })
        .await;
        assert!(
            retry(&mut seen, || owner_b.compare_and_set_root(
                current.clone(),
                Some(&previous),
                key.clone()
            ))
            .await
        );
        assert!(retry(&mut seen, || owner_b.remove_root(&item, &key)).await);
        previous = key;
        if iteration == 1 {
            // Do not race ahead of the collector: it must be collecting
            // while the remaining publications happen.
            worker::signal(&context.file("publisher-started"));
            wait_for(&context.file("collector-started")).await;
        }
    }
    write_key(&context.work, "final", &previous);
    worker::signal(&context.file("publisher-done"));
    retry(&mut seen, || owner_b.flush()).await;
    eprintln!("publisher retried {} errors: {seen:?}", seen.len());
    assert!(
        seen.iter()
            .all(|kind| matches!(kind, ErrorKind::Busy | ErrorKind::StaleRevision)),
        "{seen:?}"
    );
}

async fn race_collector(context: &Context) {
    let collector = context.remote("collector").await;
    wait_for(&context.file("publisher-started")).await;
    worker::signal(&context.file("collector-started"));
    let mut removed = 0;
    let mut passes = 0;
    let mut seen = Vec::new();
    loop {
        // Prune validation requires the exact marked revision, so a pass
        // that overlaps a publication is refused with a retryable error and
        // the only collector simply runs again. The non-blocking form never
        // waits behind collector ownership either.
        removed += retry(&mut seen, || collector.try_collect())
            .await
            .logical_objects;
        passes += 1;
        if context.file("publisher-done").exists() {
            break;
        }
    }
    eprintln!(
        "collector removed {removed} objects in {passes} passes after retrying {} errors: {seen:?}",
        seen.len()
    );
    assert!(
        seen.iter()
            .all(|kind| matches!(kind, ErrorKind::Busy | ErrorKind::StaleRevision)),
        "{seen:?}"
    );
    write_count(&context.work, "collector-removed", removed);
    // Objects superseded after the last pass are unrooted, not corrupt.
    assert!(collector.fsck().await.unwrap().is_healthy());
    collector.flush().await.unwrap();
}

async fn race_settle(context: &Context) {
    let collector = context.remote("collector").await;
    let removed = collector.collect().await.unwrap().logical_objects;
    // The seed plus every superseded item, each reclaimed exactly once.
    assert_eq!(
        read_count(&context.work, "collector-removed") + removed,
        RACE_ITERATIONS as usize
    );
    let key = read_key(&context.work, "final");
    assert_eq!(
        read_verified(&collector, &key).await,
        small(RACE_ITERATIONS)
    );
    let roots = collector.roots().await.unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name(), &name("owner-b/current"));
    assert_eq!(roots[0].target(), &key);
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
}
