//! Disk-pressure policy for scheduling generic repository collection.
//!
//! [`Repository`] owns collection correctness but does not
//! decide when a pass should run. The standard local profile applies this
//! policy when a mutation session starts; deployments may also call it from a
//! timer or pressure monitor.

use std::io;
use std::path::{Path, PathBuf};

use crate::{BlobGc, CollectionOutcome, MetadataStore, RepositoryError, repository::Repository};

/// Default pressure threshold for deployments that do not choose one.
pub const DEFAULT_DISK_PRESSURE_USED_PERCENT: u8 = 80;

/// One filesystem-capacity observation supplied to a collection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    total_bytes: u64,
    free_bytes: u64,
}

impl DiskUsage {
    /// Construct a validated observation.
    ///
    /// A zero-capacity filesystem is accepted but never treated as pressured:
    /// the observation is not meaningful enough to authorize deletion.
    pub fn new(total_bytes: u64, free_bytes: u64) -> Result<Self, RepositoryError> {
        if free_bytes > total_bytes {
            return Err(RepositoryError::InvalidInput(format!(
                "filesystem reports {free_bytes} free bytes but only {total_bytes} total bytes"
            )));
        }
        Ok(Self {
            total_bytes,
            free_bytes,
        })
    }

    /// Total filesystem capacity reported by the probe.
    pub const fn total_bytes(self) -> u64 {
        self.total_bytes
    }

    /// Free filesystem capacity reported by the probe.
    pub const fn free_bytes(self) -> u64 {
        self.free_bytes
    }

    /// Whether used capacity has reached `percent`.
    pub fn used_percent_at_least(self, percent: u8) -> bool {
        self.total_bytes != 0
            && u128::from(self.total_bytes.saturating_sub(self.free_bytes)) * 100
                >= u128::from(self.total_bytes) * u128::from(percent)
    }
}

/// Result of applying a disk-pressure policy once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskPressureOutcome {
    /// The sampled filesystem remains below the configured threshold.
    BelowThreshold {
        /// Capacity observation used for the decision.
        usage: DiskUsage,
    },
    /// Pressure reached the threshold and nonblocking collection completed.
    Collected {
        /// Capacity observation that triggered the pass.
        usage: DiskUsage,
        /// Generic repository collection result.
        outcome: CollectionOutcome,
    },
}

/// Local disk-pressure policy.
///
/// This type owns no timer or background task. The standard local profile
/// samples it at mutation-session admission, and a deployment may apply it at
/// other times. Collection uses [`Repository::try_vacuum`] so deferred pack
/// garbage is physically reclaimed; active
/// repository work produces a typed [`RepositoryError::Busy`] result instead
/// of being stalled by housekeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskPressurePolicy {
    used_percent: u8,
}

impl DiskPressurePolicy {
    /// Configure collection to begin at `used_percent` in the inclusive range
    /// `1..=100`.
    pub fn new(used_percent: u8) -> Result<Self, RepositoryError> {
        if !(1..=100).contains(&used_percent) {
            return Err(RepositoryError::InvalidInput(format!(
                "disk-pressure threshold must be between 1 and 100, got {used_percent}"
            )));
        }
        Ok(Self { used_percent })
    }

    /// Configured inclusive used-capacity threshold.
    pub const fn used_percent(self) -> u8 {
        self.used_percent
    }

    /// Apply this policy to a caller-supplied observation.
    ///
    /// Supplying the observation makes policy decisions deterministic and lets
    /// a service use its own capacity source. Use [`Self::probe_and_collect`]
    /// for the local filesystem implementation.
    #[tracing::instrument(
        name = "collection.disk_pressure",
        skip_all,
        fields(
            threshold_percent = self.used_percent,
            total_bytes = usage.total_bytes(),
            free_bytes = usage.free_bytes()
        )
    )]
    pub async fn collect_if_needed<PS, SS>(
        self,
        repository: &Repository<PS, SS>,
        usage: DiskUsage,
    ) -> Result<DiskPressureOutcome, RepositoryError>
    where
        PS: BlobGc + 'static,
        SS: MetadataStore + 'static,
    {
        if !usage.used_percent_at_least(self.used_percent) {
            tracing::debug!("disk usage remains below collection threshold");
            return Ok(DiskPressureOutcome::BelowThreshold { usage });
        }
        tracing::info!("disk pressure reached collection threshold");
        let outcome = repository.try_vacuum().await?;
        Ok(DiskPressureOutcome::Collected { usage, outcome })
    }

    /// Probe the filesystem containing `path`, then apply this policy.
    ///
    /// `path` may not exist yet; its nearest existing ancestor selects the
    /// filesystem. Capacity probing runs on Tokio's blocking pool.
    pub async fn probe_and_collect<PS, SS>(
        self,
        repository: &Repository<PS, SS>,
        path: impl AsRef<Path>,
    ) -> Result<DiskPressureOutcome, RepositoryError>
    where
        PS: BlobGc + 'static,
        SS: MetadataStore + 'static,
    {
        let usage = probe_disk_usage(path.as_ref()).await?;
        self.collect_if_needed(repository, usage).await
    }
}

impl Default for DiskPressurePolicy {
    fn default() -> Self {
        Self {
            used_percent: DEFAULT_DISK_PRESSURE_USED_PERCENT,
        }
    }
}

pub(crate) async fn probe_disk_usage(path: &Path) -> Result<DiskUsage, RepositoryError> {
    let existing = nearest_existing_ancestor(path)?;
    let (free_bytes, total_bytes) = tokio::task::spawn_blocking(move || {
        let stats = fs4::statvfs(existing)?;
        Ok::<_, io::Error>((stats.free_space(), stats.total_space()))
    })
    .await
    .map_err(|error| io::Error::other(format!("disk-capacity probe task failed: {error}")))??;
    DiskUsage::new(total_bytes, free_bytes)
}

fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf, io::Error> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Ok(candidate.to_path_buf());
        }
        candidate = candidate.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no existing filesystem ancestor for {}", path.display()),
            )
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlobStore as _, MemoryBlobStore, MemoryMetadataStore, RootName};

    fn repository() -> Repository<MemoryBlobStore, MemoryMetadataStore> {
        Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new().unwrap())
    }

    #[test]
    fn threshold_comparison_is_inclusive_and_overflow_safe() {
        let threshold = DiskPressurePolicy::new(80).unwrap();
        assert_eq!(threshold.used_percent(), 80);
        assert!(!DiskUsage::new(100, 21).unwrap().used_percent_at_least(80));
        assert!(DiskUsage::new(100, 20).unwrap().used_percent_at_least(80));
        assert!(DiskUsage::new(1, 0).unwrap().used_percent_at_least(80));
        assert!(!DiskUsage::new(0, 0).unwrap().used_percent_at_least(80));
        assert!(
            DiskUsage::new(u64::MAX, u64::MAX / 5)
                .unwrap()
                .used_percent_at_least(80)
        );
        assert!(DiskPressurePolicy::new(0).is_err());
        assert!(DiskPressurePolicy::new(101).is_err());
        assert!(DiskUsage::new(1, 2).is_err());
    }

    #[tokio::test]
    async fn zero_free_bytes_triggers_collection() {
        let repository = repository();
        let mutation = repository.mutation_session().await.unwrap();
        let orphan = mutation
            .stage_blob(b"collect when capacity reports zero free bytes")
            .await
            .unwrap();
        let key = orphan.record().key().clone();
        let payload = orphan.record().payload();
        mutation.publish_unrooted(vec![orphan]).await.unwrap();
        drop(mutation);

        let usage = DiskUsage::new(1, 0).unwrap();
        let result = DiskPressurePolicy::default()
            .collect_if_needed(&repository, usage)
            .await
            .unwrap();
        assert!(matches!(
            result,
            DiskPressureOutcome::Collected {
                outcome: CollectionOutcome {
                    removed: crate::CollectionPreview {
                        logical_objects: 1,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
        assert!(
            repository
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!repository.payloads().has(&payload).await.unwrap());
    }

    #[tokio::test]
    async fn below_threshold_does_not_collect() {
        let repository = repository();
        let mutation = repository.mutation_session().await.unwrap();
        let orphan = mutation.stage_blob(b"not pressured").await.unwrap();
        let key = orphan.record().key().clone();
        mutation.publish_unrooted(vec![orphan]).await.unwrap();
        drop(mutation);

        let usage = DiskUsage::new(100, 21).unwrap();
        assert_eq!(
            DiskPressurePolicy::default()
                .collect_if_needed(&repository, usage)
                .await
                .unwrap(),
            DiskPressureOutcome::BelowThreshold { usage }
        );
        assert!(
            repository
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&key)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn pressured_repository_collects_with_an_online_mutation() {
        let repository = repository();
        let mutation = repository.mutation_session().await.unwrap();
        let outcome = DiskPressurePolicy::default()
            .collect_if_needed(&repository, DiskUsage::new(1, 0).unwrap())
            .await
            .unwrap();
        assert!(matches!(outcome, DiskPressureOutcome::Collected { .. }));
        drop(mutation);
    }

    #[tokio::test]
    async fn probe_errors_remain_typed_backend_failures() {
        let repository = repository();
        let error = DiskPressurePolicy::default()
            .probe_and_collect(&repository, Path::new(""))
            .await
            .unwrap_err();
        assert!(matches!(error, RepositoryError::Io(_)));
    }

    #[tokio::test]
    async fn filesystem_probe_accepts_a_missing_descendant() {
        let temporary = tempfile::tempdir().unwrap();
        let usage = probe_disk_usage(&temporary.path().join("not/created/yet"))
            .await
            .unwrap();
        assert!(usage.total_bytes() > 0);
        assert!(usage.free_bytes() <= usage.total_bytes());
    }

    #[tokio::test]
    async fn rooted_data_survives_a_zero_free_pressure_pass() {
        let repository = repository();
        let mutation = repository.mutation_session().await.unwrap();
        let live = mutation.stage_blob(b"still live").await.unwrap();
        let key = live.record().key().clone();
        mutation
            .publish_rooted(
                vec![live],
                RootName::try_from("pressure/live").unwrap(),
                key.clone(),
            )
            .await
            .unwrap();
        drop(mutation);

        DiskPressurePolicy::default()
            .collect_if_needed(&repository, DiskUsage::new(1, 0).unwrap())
            .await
            .unwrap();
        assert!(
            repository
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&key)
                .await
                .unwrap()
                .is_some()
        );
    }
}
