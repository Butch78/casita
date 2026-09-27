//! Cross-process collector ownership for local repositories.
//!
//! Data lifetimes are protected by durable online pins. Only collectors and
//! initialization take this OS lock, which a process crash releases. Acquiring
//! it also proves that a previous local collector has stopped before takeover
//! of its durable deletion claims. Readers and writers never take this lock.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Error;

const GC_LOCK: &str = "gc.lock";

#[derive(Debug)]
pub(crate) struct FsCoordination {
    dir: PathBuf,
}

impl FsCoordination {
    pub(crate) fn new(store_root: impl AsRef<Path>) -> Self {
        Self {
            dir: store_root.as_ref().to_path_buf(),
        }
    }

    fn open_lock_file(&self) -> Result<File, Error> {
        std::fs::create_dir_all(&self.dir)?;
        Ok(File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join(GC_LOCK))?)
    }

    /// Reserve the lock inode while ordinary write headroom is available, so
    /// emergency collection does not need a new directory entry at ENOSPC.
    #[tracing::instrument(name = "coordination.initialize", skip_all)]
    pub(crate) async fn initialize(self: &Arc<Self>) -> Result<(), Error> {
        let coord = self.clone();
        tokio::task::spawn_blocking(move || {
            drop(coord.open_lock_file()?);
            Ok(())
        })
        .await?
    }

    /// Wait for another collector or initialization operation to finish.
    #[tracing::instrument(name = "coordination.exclusive", skip_all)]
    pub(crate) async fn exclusive(self: &Arc<Self>) -> Result<ExclusiveLease, Error> {
        let coord = self.clone();
        tokio::task::spawn_blocking(move || {
            let file = coord.open_lock_file()?;
            file.lock()?;
            Ok(ExclusiveLease { file })
        })
        .await?
    }

    /// Acquire collector ownership without waiting for a competing collector.
    #[tracing::instrument(name = "coordination.try_exclusive", skip_all)]
    pub(crate) async fn try_exclusive(self: &Arc<Self>) -> Result<Option<ExclusiveLease>, Error> {
        let coord = self.clone();
        tokio::task::spawn_blocking(move || {
            let file = coord.open_lock_file()?;
            match file.try_lock() {
                Ok(()) => Ok(Some(ExclusiveLease { file })),
                Err(std::fs::TryLockError::WouldBlock) => Ok(None),
                Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
            }
        })
        .await?
    }
}

/// Collector ownership, released even if the awaiting caller is cancelled.
pub(crate) struct ExclusiveLease {
    file: File,
}

impl Drop for ExclusiveLease {
    fn drop(&mut self) {
        // Explicit unlock also releases fork-inherited duplicate descriptors.
        // Closing is the fallback if unlock fails.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn independent_collectors_exclude_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let first = Arc::new(FsCoordination::new(directory.path()));
        let second = Arc::new(FsCoordination::new(directory.path()));
        first.initialize().await.unwrap();
        let lease = first.exclusive().await.unwrap();
        assert!(second.try_exclusive().await.unwrap().is_none());
        drop(lease);
        assert!(second.try_exclusive().await.unwrap().is_some());
        assert!(!directory.path().join("casita.lock").exists());
    }

    #[tokio::test]
    async fn blocking_collector_continues_after_owner_release() {
        let directory = tempfile::tempdir().unwrap();
        let first = Arc::new(FsCoordination::new(directory.path()));
        let second = Arc::new(FsCoordination::new(directory.path()));
        let lease = first.exclusive().await.unwrap();
        let waiter = tokio::spawn(async move { second.exclusive().await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(lease);
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(first.try_exclusive().await.unwrap().is_none());
        drop(next);
        assert!(first.try_exclusive().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_leave_collector_ownership_behind() {
        let directory = tempfile::tempdir().unwrap();
        let first = Arc::new(FsCoordination::new(directory.path()));
        let second = Arc::new(FsCoordination::new(directory.path()));
        let lease = first.exclusive().await.unwrap();
        let waiter = tokio::spawn(async move { second.exclusive().await });
        tokio::task::yield_now().await;
        waiter.abort();
        let _ = waiter.await;
        drop(lease);
        // A detached blocking acquisition must drop its result when its caller
        // is cancelled. Poll until that task has settled, with a bounded wait.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(lease) = first.try_exclusive().await.unwrap() {
                    drop(lease);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
