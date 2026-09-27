#![cfg(feature = "experimental")]

//! Traversals that outgrow memory, at the public API.
//!
//! Every test here runs the same workload twice: once with limits that keep
//! the traversal in memory, and once with limits so small that the visited set
//! and work queue must spill. The two runs must agree exactly, because
//! spilling is an implementation detail of how a traversal is stored, not of
//! what it computes.

#![cfg(feature = "native")]

use std::path::{Path, PathBuf};

use casita::experimental::{ClosureStatus, MetadataStore as _, Repository, RootName, SpillLimits};

/// Limits that force even a small traversal into local storage.
const SPILLING: SpillLimits = SpillLimits {
    max_memory_objects: 4,
    max_spill_bytes: 64 * 1024 * 1024,
};

type Local =
    Repository<casita::experimental::ChunkedBlobStore, casita::experimental::TursoMetadataStore>;

async fn repository(root: &Path, spill: Option<SpillLimits>) -> Local {
    let repository = Repository::local(root).await.unwrap();
    match spill {
        Some(limits) => repository.with_spill_limits(limits),
        None => repository,
    }
}

/// A tree with nested directories, repeated content, and enough entries to
/// outgrow a four-object memory budget many times over.
fn source_tree(root: &Path) {
    for group in 0..6 {
        let directory = root.join(format!("group-{group}"));
        std::fs::create_dir_all(directory.join("nested")).unwrap();
        for entry in 0..6 {
            std::fs::write(
                directory.join(format!("file-{entry}")),
                format!("group {group} entry {entry}").as_bytes(),
            )
            .unwrap();
            // Repeated bytes across groups: the graph shares subtrees, so a
            // spilled visited set has to deduplicate exactly like an in-memory
            // one.
            std::fs::write(
                directory.join("nested").join(format!("shared-{entry}")),
                format!("shared {entry}").as_bytes(),
            )
            .unwrap();
        }
    }
}

fn name(literal: &str) -> RootName {
    RootName::try_from(literal).unwrap()
}

/// Spill files that outlived their traversal.
fn leftover_spill(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("spill"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_spilled_traversal_agrees_with_an_in_memory_one() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    source_tree(&source);

    let memory_root = temp.path().join("in-memory");
    let spilled_root = temp.path().join("spilled");
    let memory = repository(&memory_root, None).await;
    let spilled = repository(&spilled_root, Some(SPILLING)).await;

    // A second tree of unique content, rooted and then unrooted, so collection
    // has both a live graph to mark and a dead one to remove.
    let doomed = temp.path().join("doomed");
    std::fs::create_dir_all(doomed.join("nested")).unwrap();
    for entry in 0..8 {
        std::fs::write(
            doomed.join("nested").join(format!("only-here-{entry}")),
            format!("unreachable soon {entry}").as_bytes(),
        )
        .unwrap();
    }

    let kept = name("trees/kept");
    let dropped = name("trees/dropped");
    let mut outcomes = Vec::new();
    for repository in [&memory, &spilled] {
        let root = repository
            .import(casita::import::FilesystemImport::new(&source, kept.clone()))
            .await
            .unwrap();
        let extra = repository
            .import(casita::import::FilesystemImport::new(
                &doomed,
                dropped.clone(),
            ))
            .await
            .unwrap();
        let ClosureStatus::Complete { objects } = repository.verify_closure(&root).await.unwrap()
        else {
            panic!("the imported closure must be complete");
        };
        repository
            .remove_root_if_matches(&dropped, &extra)
            .await
            .unwrap()
            .expect("the extra root still matches");
        let preview = repository.preview_collection().await.unwrap();
        let collected = repository.collect().await.unwrap();
        assert!(repository.fsck().await.unwrap().is_healthy());
        let snapshot = repository.metadata().snapshot().await.unwrap();
        assert_eq!(snapshot.root(&kept).await.unwrap(), Some(root.clone()));
        assert_eq!(snapshot.root(&dropped).await.unwrap(), None);
        outcomes.push((root, objects, preview, collected.removed));
    }

    let (memory_outcome, spilled_outcome) = (&outcomes[0], &outcomes[1]);
    assert_eq!(memory_outcome.0, spilled_outcome.0, "root identity");
    assert_eq!(memory_outcome.1, spilled_outcome.1, "closure size");
    assert_eq!(memory_outcome.2, spilled_outcome.2, "collection preview");
    assert_eq!(memory_outcome.3, spilled_outcome.3, "collected counts");
    assert!(memory_outcome.1 > SPILLING.max_memory_objects);
    assert!(
        memory_outcome.3.logical_objects > 0,
        "nothing was collected"
    );

    // The spilling run really did reach for storage, and left none behind.
    assert!(
        spilled_root.join("spill").is_dir(),
        "the spilling run never opened its spill area"
    );
    assert!(leftover_spill(&spilled_root).is_empty());
    assert!(
        !memory_root.join("spill").is_dir(),
        "an in-memory traversal must not create spill state"
    );
}

#[tokio::test]
async fn a_spilled_export_plan_produces_the_same_archive() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    source_tree(&source);

    let memory_root = temp.path().join("in-memory");
    let spilled_root = temp.path().join("spilled");
    let memory = repository(&memory_root, None).await;
    let spilled = repository(&spilled_root, Some(SPILLING)).await;

    let exported = name("trees/exported");
    let mut archives = Vec::new();
    for repository in [&memory, &spilled] {
        repository
            .import(casita::import::FilesystemImport::new(
                &source,
                exported.clone(),
            ))
            .await
            .unwrap();
        let (archive, report) = repository
            .export_casitar(
                [exported.clone()],
                Vec::new(),
                casita::experimental::CasitarStreamLimits::default(),
            )
            .await
            .unwrap();
        assert!(report.stats.records > 1);
        archives.push(archive);
    }

    // Casitar framing is deterministic, so a plan that spilled must produce the
    // same bytes as one that did not.
    assert_eq!(archives[0], archives[1]);
    assert!(spilled_root.join("spill").is_dir());
    assert!(leftover_spill(&spilled_root).is_empty());
}

#[tokio::test]
async fn fsck_reports_actual_spill_telemetry() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    source_tree(&source);
    let root = temp.path().join("repository");
    let repository = repository(&root, Some(SPILLING)).await;
    repository
        .import(casita::import::FilesystemImport::new(
            &source,
            name("trees/telemetry"),
        ))
        .await
        .unwrap();

    let report = repository.fsck().await.unwrap();
    assert!(report.is_healthy());
    assert!(
        report.spill.files_opened > 0,
        "fsck never opened spill state"
    );
    assert!(
        report.spill.peak_bytes > 0,
        "fsck never measured spill bytes"
    );
    assert!(leftover_spill(&root).is_empty());
}

#[tokio::test]
async fn a_traversal_that_outgrows_its_temporary_budget_fails() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    source_tree(&source);

    let root = temp.path().join("repository");
    let repository = repository(&root, None).await;
    repository
        .import(casita::import::FilesystemImport::new(
            &source,
            name("trees/too-big"),
        ))
        .await
        .unwrap();

    // Filesystem construction proves the imported closure without traversing
    // it a second time. Exercise an explicitly exhaustive traversal instead.
    let repository = repository.with_spill_limits(SpillLimits {
        max_memory_objects: 4,
        // One byte cannot hold a spilled visited set.
        max_spill_bytes: 1,
    });
    let error = repository
        .fsck()
        .await
        .expect_err("a one byte spill budget cannot hold this traversal");
    assert!(matches!(
        error,
        casita::experimental::RepositoryError::Payload(casita::experimental::Error::LimitExceeded(
            _
        ))
    ));
    assert!(
        error.to_string().contains("limit is 1"),
        "unexpected error: {error}"
    );
    assert!(
        leftover_spill(&root).is_empty(),
        "a failure leaked spill state"
    );
}

#[tokio::test]
async fn spill_state_from_a_dead_process_is_swept_at_open() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    drop(repository(&root, None).await);

    // What a killed process leaves behind: files whose lock nobody holds.
    let spill = root.join("spill");
    std::fs::create_dir_all(&spill).unwrap();
    std::fs::write(spill.join("closure-424242-0.sqlite"), b"abandoned").unwrap();
    std::fs::write(spill.join("closure-424242-0.lock"), b"").unwrap();

    let reopened = repository(&root, None).await;
    assert!(
        leftover_spill(&root).is_empty(),
        "stale spill state survived"
    );
    assert!(reopened.fsck().await.unwrap().is_healthy());
}
