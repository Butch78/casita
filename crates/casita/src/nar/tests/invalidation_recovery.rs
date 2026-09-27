use super::*;
use crate::metadata::{FactsEdit, MetadataError, MetadataStore, VerificationFacts};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

// Fail or suspend on either side of the backend commit. A caller cannot tell
// whether an interrupted write committed, so recovery must handle both.
struct InterruptedFacts {
    inner: Arc<dyn VerificationFacts>,
    fault: AtomicU8,
    clears: AtomicUsize,
    suspended: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    pause_edit: AtomicBool,
}

#[async_trait::async_trait]
impl VerificationFacts for InterruptedFacts {
    fn scope(&self) -> Option<PathBuf> {
        self.inner.scope()
    }
    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, MetadataError> {
        self.inner.get(key).await
    }
    async fn edit(&self, keys: Vec<Vec<u8>>, edit: FactsEdit) -> Result<(), MetadataError> {
        self.inner.edit(keys, edit).await?;
        if self.pause_edit.swap(false, Ordering::SeqCst) {
            self.suspended.notify_one();
            self.resume.notified().await;
        }
        Ok(())
    }
    async fn clear(&self, tombstone: Vec<u8>, generation: Vec<u8>) -> Result<(), MetadataError> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        let fault = self.fault.swap(0, Ordering::SeqCst);
        if matches!(fault, 0 | 2 | 4 | 5) {
            self.inner.clear(tombstone, generation).await?;
        }
        match fault {
            0 => Ok(()),
            1 | 2 => Err(MetadataError::StorageFull),
            3 | 4 => {
                self.suspended.notify_one();
                std::future::pending().await
            }
            5 => {
                self.suspended.notify_one();
                self.resume.notified().await;
                Ok(())
            }
            _ => unreachable!(),
        }
    }
    async fn page(&self, after: Vec<u8>, limit: usize) -> Result<Vec<Vec<u8>>, MetadataError> {
        self.inner.page(after, limit).await
    }
}

async fn interrupted(
    local: bool,
    fault: u8,
) -> (
    tempfile::TempDir,
    Repository,
    VerifiedNarReport,
    Arc<InterruptedFacts>,
    u64,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = if local {
        Repository::local(dir.path()).await.unwrap()
    } else {
        Repository::memory().unwrap()
    };
    let report = repo.import(NarImport::new(HELLO)).await.unwrap();
    let facts = Arc::new(InterruptedFacts {
        inner: repo.inner.metadata().verification_facts().unwrap(),
        fault: AtomicU8::new(0),
        clears: AtomicUsize::new(0),
        suspended: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        pause_edit: AtomicBool::new(false),
    });
    let store = store::NarStore::new(facts.clone());
    repo.inner.nar_store = Some(store.clone());
    let before = store.generation().await.unwrap();
    store
        .merge(b"retired", &report.facts, before)
        .await
        .unwrap();
    store
        .set_witness(b"retired", b"old witness".to_vec())
        .await
        .unwrap();
    store.quarantine(b"conflict").await.unwrap();
    facts.fault.store(fault, Ordering::SeqCst);
    if fault <= 2 {
        assert!(store.invalidate().await.is_err());
    } else {
        let invalidation = store.invalidate();
        tokio::pin!(invalidation);
        tokio::select! {
            _ = facts.suspended.notified() => {},
            result = &mut invalidation => panic!("invalidation did not suspend: {result:?}"),
        }
        // Dropping the future releases its lock but leaves the handle closed.
    }
    assert!(store.requires_native_audit().await.unwrap());
    assert!(store.get(&identity(report.root())).await.unwrap().is_none());
    assert!(store.witness(b"retired").await.unwrap().is_none());
    assert!(!store.restore(before).await.unwrap());
    (dir, repo, report, facts, before)
}

#[tokio::test]
async fn queued_invalidation_survives_cancellation_after_an_earlier_clear_commits() {
    for local in [false, true] {
        let (_dir, repo, report, facts, _) = interrupted(local, 1).await;
        let store = repo.inner.nar_store.as_ref().unwrap();
        facts.fault.store(5, Ordering::SeqCst);
        let mut first = Box::pin(store.invalidate());
        tokio::select! {
            _ = facts.suspended.notified() => {},
            result = &mut first => panic!("clear did not suspend: {result:?}"),
        }
        // The earlier clear has committed, but still holds the handle lock.
        // Another handle can begin an audit and verify facts at this point.
        let other = store::NarStore::new(facts.inner.clone());
        let audit_generation = other.generation().await.unwrap();
        other
            .merge(b"late", &report.facts, audit_generation)
            .await
            .unwrap();
        other
            .set_witness(b"late", b"late witness".to_vec())
            .await
            .unwrap();
        let mut queued = Box::pin(store.invalidate());
        assert!(futures::poll!(queued.as_mut()).is_pending());
        drop(queued);
        facts.resume.notify_one();
        first.await.unwrap();

        // Completing the earlier request must also durably cover the queued
        // notification. Neither handle's earlier audit may clear its marker.
        assert!(!store.restore(audit_generation).await.unwrap());
        assert!(!other.restore(audit_generation).await.unwrap());
        assert!(other.requires_native_audit().await.unwrap());
        assert!(other.get(b"late").await.unwrap().is_none());
        assert!(other.witness(b"late").await.unwrap().is_none());
        assert_eq!(other.generation().await.unwrap(), audit_generation + 1);
        assert_eq!(facts.clears.load(Ordering::SeqCst), 3);
        assert!(
            store
                .restore(other.generation().await.unwrap())
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn queued_invalidation_survives_cancellation_behind_audit_restoration() {
    for local in [false, true] {
        let (_dir, repo, report, facts, _) = interrupted(local, 1).await;
        let store = repo.inner.nar_store.as_ref().unwrap();
        let generation = store.generation().await.unwrap();
        store
            .merge(b"late", &report.facts, generation)
            .await
            .unwrap();
        facts.pause_edit.store(true, Ordering::SeqCst);
        let mut audit = Box::pin(store.restore(generation));
        tokio::select! {
            _ = facts.suspended.notified() => {},
            result = &mut audit => panic!("restoration did not suspend: {result:?}"),
        }
        let mut queued = Box::pin(store.invalidate());
        assert!(futures::poll!(queued.as_mut()).is_pending());
        drop(queued);
        facts.resume.notify_one();
        // This audit committed before the new report, but its completion must
        // not acknowledge an invalidation that arrived while it held the lock.
        assert!(audit.await.unwrap());
        assert!(store.requires_native_audit().await.unwrap());
        assert!(store.get(b"late").await.unwrap().is_none());
        assert!(!store.restore(generation).await.unwrap());
        let next = store.generation().await.unwrap();
        assert_eq!(next, generation + 1);
        assert!(store.requires_native_audit().await.unwrap());
        assert!(store.get(b"late").await.unwrap().is_none());
        assert!(store.restore(next).await.unwrap());
    }
}

#[tokio::test]
async fn interrupted_invalidation_recovers_measurements() {
    for local in [false, true] {
        for fault in 1..=4 {
            let (dir, repo, report, facts, before) = interrupted(local, fault).await;
            let store = repo.inner.nar_store.as_ref().unwrap();
            let reader = repo.retained_reader().await.unwrap();
            // A second backend failure must still suppress every cached fact.
            facts.fault.store(1, Ordering::SeqCst);
            assert!(
                ensure_nar(&reader, report.root(), &NarRequirements::default())
                    .await
                    .is_err()
            );
            assert!(store.requires_native_audit().await.unwrap());
            assert!(store.get(&identity(report.root())).await.unwrap().is_none());
            assert!(!store.restore(before).await.unwrap());

            let measured = ensure_nar(&reader, report.root(), &NarRequirements::default())
                .await
                .unwrap();
            assert_eq!(measured.nar_sha256(), report.nar_sha256());
            assert_eq!(measured.stats().encoding_passes, 1);
            assert_eq!(measured.stats().hash_payload_bytes, 6);
            assert!(
                ensure_nar(&reader, report.root(), &NarRequirements::default())
                    .await
                    .unwrap()
                    .stats()
                    .association_hit
            );
            assert_eq!(facts.clears.load(Ordering::SeqCst), 3);
            assert!(store.get(b"retired").await.unwrap().is_none());
            assert!(store.witness(b"retired").await.unwrap().is_none());
            assert!(matches!(
                store.get(b"conflict").await,
                Err(NarError::Conflict)
            ));
            assert!(!store.restore(before).await.unwrap());
            assert!(
                store
                    .merge(&identity(report.root()), &report.facts, before)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(store.requires_native_audit().await.unwrap());
            let expected = before + if matches!(fault, 2 | 4) { 2 } else { 1 };
            assert_eq!(store.generation().await.unwrap(), expected);
            if local {
                let other = Repository::local(dir.path()).await.unwrap();
                let other = other.inner.nar_store.as_ref().unwrap();
                assert_eq!(other.generation().await.unwrap(), expected);
                assert!(other.requires_native_audit().await.unwrap());
                assert!(!other.restore(before).await.unwrap());
            }
        }
    }
}

#[tokio::test]
async fn interrupted_invalidation_recovery_can_be_cancelled_and_retried() {
    for local in [false, true] {
        let (_dir, repo, _report, facts, before) = interrupted(local, 1).await;
        let store = repo.inner.nar_store.as_ref().unwrap();
        facts.fault.store(3, Ordering::SeqCst);
        let mut first = Box::pin(store.generation());
        tokio::select! {
            _ = facts.suspended.notified() => {},
            result = &mut first => panic!("recovery did not suspend: {result:?}"),
        }
        let mut second = Box::pin(store.generation());
        let mut third = Box::pin(store.generation());
        assert!(futures::poll!(second.as_mut()).is_pending());
        assert!(futures::poll!(third.as_mut()).is_pending());
        drop(first);
        assert!(store.requires_native_audit().await.unwrap());
        let (second, third) = tokio::join!(second, third);
        assert_eq!(second.unwrap(), before + 1);
        assert_eq!(third.unwrap(), before + 1);
        // Initial failure, cancelled recovery, then one successful clear.
        assert_eq!(facts.clears.load(Ordering::SeqCst), 3);
        assert!(!store.restore(before).await.unwrap());
        assert!(store.requires_native_audit().await.unwrap());
        assert!(store.get(b"retired").await.unwrap().is_none());
    }
}

#[tokio::test]
async fn interrupted_invalidation_recovers_intake_and_audits() {
    for local in [false, true] {
        for fault in 1..=4 {
            for audit in [false, true] {
                if audit && !local {
                    continue; // Physical fsck is a local chunk-store operation.
                }
                let (dir, repo, report, facts, before) = interrupted(local, fault).await;
                let store = repo.inner.nar_store.as_ref().unwrap();
                if audit {
                    let mut core = crate::repository::Repository::local(dir.path())
                        .await
                        .unwrap();
                    core.nar_store = Some(store.clone());
                    let scan = core.fsck_repair(None).await.unwrap();
                    assert!(scan.findings.is_empty());
                    assert!(!store.requires_native_audit().await.unwrap());
                }
                let imported = repo.import(NarImport::new(HELLO)).await.unwrap();
                assert_eq!(imported.nar_sha256(), report.nar_sha256());
                assert_eq!(imported.stats().encoding_passes, u64::from(!audit));
                assert_eq!(
                    imported.stats().hash_payload_bytes,
                    if audit { 6 } else { 12 }
                );
                assert!(
                    ensure_nar(
                        imported.reader(),
                        imported.root(),
                        &NarRequirements::default()
                    )
                    .await
                    .unwrap()
                    .stats()
                    .association_hit
                );
                assert_eq!(facts.clears.load(Ordering::SeqCst), 2);
                assert!(store.get(b"retired").await.unwrap().is_none());
                assert!(store.witness(b"retired").await.unwrap().is_none());
                assert!(!store.restore(before).await.unwrap());
            }
        }
    }
}
