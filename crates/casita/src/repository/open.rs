//! Built-in repository profiles and local mutation scheduling.

use super::*;

impl Repository<crate::MemoryBlobStore, crate::MemoryMetadataStore> {
    /// Create an ephemeral full repository.
    pub fn memory() -> Result<Self, RepositoryError> {
        Ok(Self::new(
            crate::MemoryBlobStore::new(),
            crate::MemoryMetadataStore::new()?,
        ))
    }
}

impl Repository<crate::ChunkedBlobStore, crate::TursoMetadataStore> {
    /// Open the standard persistent local profile under `root`.
    ///
    /// Payloads live below `<root>/blobs`, revisioned logical state lives in
    /// `<root>/casita.sqlite`, and lock files under `root` coordinate
    /// mutation, stable reads, and collection across processes. Repeated pack
    /// reads use the default bounded compressed-chunk cache.
    pub async fn local(root: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        Self::local_with_pack_options(root, crate::PackOptions::local()).await
    }

    /// Open the local profile with explicit immutable-pack cache tuning.
    ///
    /// The cache is process-local and bounded. Set `options.cache_capacity` to
    /// zero to disable payload caching.
    #[tracing::instrument(
        name = "repository.open",
        skip_all,
        fields(
            profile = "local",
            pack_target_bytes = options.target_size,
            pack_cache_bytes = options.cache_capacity,

        )
    )]
    pub async fn local_with_pack_options(
        root: impl AsRef<Path>,
        options: crate::PackOptions,
    ) -> Result<Self, RepositoryError> {
        let root = root.as_ref().to_path_buf();
        let payload_root = root.join("blobs");
        std::fs::create_dir_all(&payload_root)?;
        let profile = RepositoryProfile::local(&root).await?;
        let fs_coordination = profile
            .fs_coordination()
            .expect("the local profile coordinates across processes")
            .clone();
        // Initialization and supported format migrations may write. Keep them
        // in the same cross-process domain as every other state mutation.
        // Already-migrated repositories open without a migration write, so
        // collection can still be the operation that recovers disk space.
        let initialization_lease = fs_coordination.exclusive().await?;
        let db = tokio::task::spawn_blocking({
            let path = root.join("casita.sqlite");
            move || crate::sqlite::TursoDb::open(path)
        })
        .await
        .map_err(|error| RepositoryError::Payload(error.into()))??;
        let state = crate::TursoMetadataStore::from_db(db).await?;
        let snapshot = state.snapshot().await?;
        let catalog = snapshot.payload_catalog().map(ToOwned::to_owned);
        if catalog.is_none() && snapshot.objects().next().await.is_some() {
            return Err(MetadataError::Corruption(
                "local repository state does not contain a payload catalog".to_owned(),
            )
            .into());
        }
        let revision = snapshot.revision();
        drop(snapshot);
        let catalog = catalog.unwrap_or(crate::ChunkedBlobStore::empty_state_catalog()?);
        // The SQLite revision is authoritative, including after an interrupted
        // publication. Never fall back to a standalone pointer or inventory.
        let payloads = crate::ChunkedBlobStore::local_packed_with_catalog(
            payload_root,
            options,
            Some(&catalog),
        )
        .await?;
        // Schema 6 fences old publication SQL before this migration begins.
        // The exclusive repository lease prevents GC from racing the upload.
        // Reopening after interruption retries from the still-authoritative
        // inline witness, or reads the already durable external root.
        // Keep the lease with the cancellation-safe task: the SQLite write may
        // outlive an aborted open, and GC must not delete its candidate root.
        let (payloads, state) =
            crate::metadata::run_lease_task("catalog migration", |send| async move {
                let _lease = initialization_lease;
                let result = async {
                    let external = payloads.externalize_state_catalog(&catalog).await?;
                    if external != catalog {
                        #[cfg(test)]
                        crate::blob::crash_tests::checkpoint("before-catalog-migration");
                        let mut mutation = MetadataMutation::new();
                        mutation.set_payload_catalog(external.clone());
                        state.commit(&revision, mutation).await?;
                        #[cfg(test)]
                        crate::blob::crash_tests::checkpoint("after-catalog-migration");
                        payloads
                            .publication()
                            .synchronize_state_catalog(Some(&external))
                            .await?;
                    }
                    Ok::<_, RepositoryError>((payloads, state))
                }
                .await;
                let _ = send.send(result);
                Ok(())
            })
            .await??;
        // Only the standard local profile keeps an ingest cache: it is the
        // profile that imports from a filesystem whose stat data means
        // anything.
        let profile = profile.with_ingest_cache(&state);
        let repository = Self::new(payloads, state).with_profile(profile);
        // Reuse the online catalog collector when opening finds abandoned
        // metadata. An existing owner or recovery claim leaves cleanup for a
        // later collection without preventing the repository from opening.
        match repository.try_reclaim_metadata().await {
            Ok(_) | Err(RepositoryError::Busy(_)) => {}
            Err(error) => return Err(error),
        }
        tracing::info!(profile = "local", "repository opened");
        Ok(repository)
    }
}

#[cfg(feature = "s3")]
impl Repository<crate::ChunkedBlobStore, crate::metadata::Wal3MetadataStore> {
    /// Open a repository whose payloads and revisioned state are authoritative
    /// in one S3 bucket.
    ///
    /// Every runner uses the same `bucket` and repository `prefix`, but gives
    /// itself a distinct diagnostic `writer` name. AWS credentials and region
    /// are read from standard `AWS_*` environment variables by both physical
    /// storage clients. Durable online pins protect scoped data while physical
    /// collection proceeds across runners. Drop
    /// sessions and call [`crate::flush_repository_leases`] before runtime
    /// shutdown. All runners must support this admission protocol.
    pub async fn s3(
        bucket: impl AsRef<str>,
        prefix: impl AsRef<str>,
        writer: impl Into<String>,
    ) -> Result<Self, RepositoryError> {
        Self::s3_with_pack_options(bucket, prefix, writer, crate::PackOptions::default()).await
    }

    /// Open the S3/wal3 profile with explicit immutable-pack tuning.
    ///
    /// This is intended for measured deployment tuning. Set
    /// `options.cache_capacity` to zero to disable payload caching.
    #[tracing::instrument(
        name = "repository.open",
        skip_all,
        fields(
            profile = "s3",
            pack_target_bytes = options.target_size,
            pack_cache_bytes = options.cache_capacity,

        )
    )]
    pub async fn s3_with_pack_options(
        bucket: impl AsRef<str>,
        prefix: impl AsRef<str>,
        writer: impl Into<String>,
        options: crate::PackOptions,
    ) -> Result<Self, RepositoryError> {
        Self::open_s3_for_collection_recovery(bucket, prefix, writer, options, None)
            .await
            .map(|(repository, _)| repository)
    }

    /// Reopen and resume an abandoned S3 collection, including a prune fence
    /// that prevents ordinary repository admission.
    ///
    /// Establish that all requests covered by the abandoned collector's claims
    /// have stopped, then recover its operational WAL3 token separately. Supply
    /// the exact collector token from the state pin ledger. This method owns
    /// collector admission before opening payload discovery and retains that
    /// ownership through recovery; it never admits an unprotected data reader.
    pub async fn recover_s3_collection(
        bucket: impl AsRef<str>,
        prefix: impl AsRef<str>,
        writer: impl Into<String>,
        abandoned: &crate::metadata::PinToken,
    ) -> Result<CollectionOutcome, RepositoryError> {
        let (repository, remote) = Self::open_s3_for_collection_recovery(
            bucket,
            prefix,
            writer,
            crate::PackOptions::default(),
            Some(abandoned),
        )
        .await?;
        let guard = repository.coordination.clone().lock_owned().await;
        let plan = repository
            .collection_plan_with_recovery(guard, None, false, Some(abandoned), remote)
            .await?;
        repository.execute_collection(plan, true).await
    }

    pub(super) async fn open_s3_for_collection_recovery(
        bucket: impl AsRef<str>,
        prefix: impl AsRef<str>,
        writer: impl Into<String>,
        options: crate::PackOptions,

        recovery: Option<&crate::metadata::PinToken>,
    ) -> Result<(Self, Option<crate::metadata::RepositoryLease>), RepositoryError> {
        let bucket = bucket.as_ref().to_owned();
        let prefix = prefix.as_ref().trim_matches('/');
        let payload_prefix = if prefix.is_empty() {
            "payloads".to_owned()
        } else {
            format!("{prefix}/payloads")
        };
        let state_prefix = if prefix.is_empty() {
            "state".to_owned()
        } else {
            format!("{prefix}/state")
        };
        let objects = crate::object_store::aws::AmazonS3Builder::from_env()
            .with_bucket_name(&bucket)
            // Keep transport integrity explicit even when an installation
            // opts into unsigned SigV4 payloads. S3 validates this checksum
            // as part of the existing PUT/multipart request, so this costs no
            // additional object-store operation.
            .with_checksum_algorithm(crate::object_store::aws::Checksum::SHA256)
            .build()
            .map_err(crate::error::Error::from)?;
        // WAL3 is authoritative for the exact payload catalog. Recover it
        // first so a normal reopen can skip the advisory pointer entirely.
        let state =
            crate::metadata::Wal3MetadataStore::open_s3(bucket, state_prefix, writer).await?;
        let (snapshot, _bootstrap_pin, remote) = if let Some(expected) = recovery {
            let remote = state.try_collection_lease().await?.ok_or_else(|| {
                RepositoryError::Busy("another collector owns S3 repository admission".into())
            })?;
            if state
                .pin_store()
                .await?
                .inventory()
                .await?
                .collector
                .as_ref()
                != Some(expected)
            {
                return Err(RepositoryError::Busy(
                    "abandoned collector token no longer matches the pin ledger".into(),
                ));
            }
            // Collector ownership excludes metadata and payload reclamation.
            // Taking a normal snapshot pin here would block on the old fence
            // and retain the very records that recovery needs to prune.
            (state.snapshot().await?, None, Some(remote))
        } else {
            let (snapshot, pin) = retention::pin_metadata_snapshot(&state, true, None).await?;
            (snapshot, Some(pin), None)
        };
        let catalog = snapshot.payload_catalog().ok_or_else(|| {
            MetadataError::Corruption(
                "WAL3 repository state does not contain a payload catalog".to_owned(),
            )
        })?;
        let payloads = crate::ChunkedBlobStore::packed_with_catalog(
            Arc::new(objects),
            crate::object_store::path::Path::from(payload_prefix),
            crate::blob::DEFAULT_AVG_CHUNK_SIZE,
            options,
            catalog,
        )
        .await?;
        drop(snapshot);
        tracing::info!(profile = "s3", "repository opened");
        Ok((Self::new(payloads, state), remote))
    }
}

#[async_trait]
pub(super) trait MutationStart: Send + Sync {
    /// Run local maintenance and report whether it refreshed payload discovery.
    async fn before_mutation(&self, spill_limits: SpillLimits) -> Result<bool, RepositoryError>;

    fn collection_completed(&self) {}
}

/// The standard local profile's bounded metadata maintenance and
/// pressure-driven collector.
///
/// The cloned repository's profile intentionally carries no mutation start
/// hook, so entering collection here cannot recurse back into this policy.
pub(super) struct LocalMutationStart<PS, SS> {
    pub(super) repository: Repository<PS, SS>,
    pub(super) root: PathBuf,
    pub(super) reclaim_countdown: std::sync::atomic::AtomicUsize,
    pub(super) attempt: tokio::sync::Mutex<()>,
}

const PRESSURE_COLLECTION_STAMP: &str = ".casita-pressure-collection";
const PRESSURE_COLLECTION_COOLDOWN: Duration = Duration::from_secs(60);

impl<PS, SS> LocalMutationStart<PS, SS> {
    pub(super) fn new(repository: Repository<PS, SS>, root: PathBuf) -> Self {
        Self {
            repository,
            root,
            reclaim_countdown: std::sync::atomic::AtomicUsize::new(0),
            attempt: tokio::sync::Mutex::new(()),
        }
    }

    pub(super) fn collection_is_recent(&self) -> bool {
        let Ok(modified) = std::fs::metadata(self.root.join(PRESSURE_COLLECTION_STAMP))
            .and_then(|metadata| metadata.modified())
        else {
            return false;
        };
        modified
            .elapsed()
            .map_or(true, |elapsed| elapsed < PRESSURE_COLLECTION_COOLDOWN)
    }

    pub(super) fn record_collection(&self) {
        // This is an advisory latency guard, not repository state. A failed
        // timestamp write merely allows a later process to try collection
        // again; it cannot make committed content unreachable.
        let _ = std::fs::write(
            self.root.join(PRESSURE_COLLECTION_STAMP),
            b"pressure-triggered collection completed\n",
        );
    }
}

#[async_trait]
impl<PS, SS> MutationStart for LocalMutationStart<PS, SS>
where
    PS: BlobGc + 'static,
    SS: MetadataStore + 'static,
{
    #[tracing::instrument(name = "repository.mutation.prepare", level = "debug", skip_all)]
    async fn before_mutation(&self, spill_limits: SpillLimits) -> Result<bool, RepositoryError> {
        use std::sync::atomic::Ordering;

        // Historical catalog pins can keep the persistent reclaim marker set
        // after a successful sweep. Share a bounded deferral across clones to
        // avoid paying for the same sweep on every mutation. A busy or failed
        // attempt does not start the deferral; reopen and vacuum bypass it.
        let reclaim_due = self
            .reclaim_countdown
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_err();
        let mut discovery_refreshed = if reclaim_due {
            match self.repository.try_reclaim_metadata().await {
                Ok(refreshed) => {
                    if refreshed {
                        self.reclaim_countdown
                            .store(METADATA_RECLAIM_INTERVAL - 1, Ordering::Release);
                    }
                    refreshed
                }
                Err(RepositoryError::Busy(_)) => false,
                Err(error) => return Err(error),
            }
        } else {
            false
        };

        if self.collection_is_recent() {
            return Ok(discovery_refreshed);
        }

        let _attempt = self.attempt.lock().await;
        if self.collection_is_recent() {
            return Ok(discovery_refreshed);
        }

        let repository = self.repository.clone().with_spill_limits(spill_limits);
        match crate::DiskPressurePolicy::default()
            .probe_and_collect(&repository, &self.root)
            .await
        {
            Ok(crate::DiskPressureOutcome::BelowThreshold { .. }) => {}
            Ok(crate::DiskPressureOutcome::Collected { .. }) => {
                let released = repository.evict_roots_under_pressure(&self.root).await?;
                tracing::info!(released, "disk-pressure root eviction completed");
                self.record_collection();
                discovery_refreshed = true;
            }
            Err(RepositoryError::Busy(_)) => {}
            Err(error) => return Err(error),
        }
        Ok(discovery_refreshed)
    }

    fn collection_completed(&self) {
        self.record_collection();
    }
}
