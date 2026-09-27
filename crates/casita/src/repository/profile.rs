//! Deployment policy a repository applies on top of its backends.

use super::*;

/// How a repository is deployed, separate from which backends hold its
/// payloads and revisioned state.
///
/// A profile decides whether collection ownership is coordinated with other
/// processes, where traversal state spills and how far it may grow, whether a
/// full disk may be relieved by deleting stale payloads, whether imports
/// remember the files they read, and whether mutations run the local
/// disk-pressure maintenance.
///
/// [`generic`](Self::generic) is what [`Repository::new`] starts from: no
/// cross-process coordination, no local accelerators and no implicit
/// maintenance. [`local`](Self::local) is the policy of
/// [`Repository::local`], so a caller composing its own backends below one
/// directory can apply it with [`Repository::with_profile`].
#[derive(Clone)]
pub struct RepositoryProfile {
    coordination: Option<Arc<FsCoordination>>,
    /// Where traversal state spills once it outgrows memory. `None` selects
    /// the platform temporary directory.
    spill_directory: Option<PathBuf>,
    spill_limits: SpillLimits,
    /// The deployment colocates state and payloads on one filesystem, so
    /// deleting stale payloads can fund a state write that ran out of space.
    emergency_collection: bool,
    /// Remembers which content each imported file held, so an unchanged file
    /// is not read again. Never something a reader depends on.
    ingest_cache: Option<Arc<dyn crate::filesystem::IngestCache>>,
    /// Filesystem whose usage decides when a mutation first collects.
    pressure_collection_root: Option<PathBuf>,
    /// The hook [`Repository::with_profile`] instantiates from the policy
    /// above. It holds its own repository clone, so it is never part of a
    /// profile a caller builds.
    pub(super) mutation_start: Option<Arc<dyn MutationStart>>,
}

impl Default for RepositoryProfile {
    fn default() -> Self {
        Self::generic()
    }
}

impl RepositoryProfile {
    /// The profile of a composition that makes no assumption about where its
    /// backends live.
    pub fn generic() -> Self {
        Self {
            coordination: None,
            spill_directory: None,
            spill_limits: SpillLimits::default(),
            emergency_collection: false,
            ingest_cache: None,
            pressure_collection_root: None,
            mutation_start: None,
        }
    }

    /// The standard local profile for backends colocated below `root`.
    ///
    /// Collection ownership is coordinated through lock files under `root`,
    /// traversals spill below it, a full disk may be relieved by deleting
    /// stale payloads, and the first mutation of each handle collects when the
    /// filesystem holding `root` is under pressure. Every process opening the
    /// backends must use the same `root`.
    ///
    /// Opening reserves the collector lock file while space is available and
    /// removes spill files left by traversals whose process has exited. Add
    /// the import cache with [`with_ingest_cache`](Self::with_ingest_cache)
    /// when the state backend is a [`TursoMetadataStore`](crate::TursoMetadataStore).
    pub async fn local(root: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let root = root.as_ref();
        let profile = Self {
            emergency_collection: true,
            pressure_collection_root: Some(root.to_path_buf()),
            ..Self::generic()
        }
        .with_fs_coordination(root);
        if let Some(coordination) = &profile.coordination {
            coordination.initialize().await?;
        }
        // Traversal state that died with an earlier process is removed here,
        // while a spill still held by a live traversal in another process is
        // left alone.
        if let Some(spill_directory) = profile.spill_directory.clone() {
            tokio::task::spawn_blocking(move || SpillArea::sweep_stale(&spill_directory))
                .await
                .map_err(|error| RepositoryError::Payload(error.into()))?;
        }
        Ok(profile)
    }

    /// Coordinate retention and collection ownership with every process
    /// opening the same local `root`, and spill traversal state below it so
    /// its bytes are accounted against the repository's own filesystem.
    pub fn with_fs_coordination(mut self, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        self.coordination = Some(Arc::new(FsCoordination::new(root)));
        self.spill_directory = Some(root.join(crate::spill::SPILL_DIRECTORY));
        self
    }

    /// Bound the memory and temporary bytes a traversal may use before, and
    /// after, it spills to local storage.
    pub fn with_spill_limits(mut self, limits: SpillLimits) -> Self {
        self.spill_limits = limits;
        self
    }

    /// Remember imported file identities in `metadata`'s database, so a
    /// re-import skips reading files that have not changed. Collection keeps
    /// the remembered entries consistent with that store's state, so the
    /// repository must use `metadata` as its state backend.
    pub fn with_ingest_cache(mut self, metadata: &crate::TursoMetadataStore) -> Self {
        self.ingest_cache = Some(Arc::new(crate::filesystem::cache::TursoIngestCache::new(
            metadata.database().clone(),
        )));
        self
    }

    /// Active traversal spill bounds.
    pub fn spill_limits(&self) -> SpillLimits {
        self.spill_limits
    }

    pub(super) fn fs_coordination(&self) -> Option<&Arc<FsCoordination>> {
        self.coordination.as_ref()
    }

    /// Whether other processes may change the payload inventory underneath
    /// this handle, so retained reads must refresh it.
    pub(super) fn coordinates_processes(&self) -> bool {
        self.coordination.is_some()
    }

    /// Whether storage exhaustion may be relieved by deleting stale payloads.
    pub(super) fn collects_in_emergency(&self) -> bool {
        self.emergency_collection
    }

    pub(super) fn ingest_cache(&self) -> Option<&Arc<dyn crate::filesystem::IngestCache>> {
        self.ingest_cache.as_ref()
    }

    pub(super) fn mutation_start(&self) -> Option<&Arc<dyn MutationStart>> {
        self.mutation_start.as_ref()
    }

    pub(super) fn spill_area(&self) -> SpillArea {
        SpillArea::new(self.spill_directory.clone(), self.spill_limits)
    }

    /// Instantiate the profile's mutation start hook for `repository`, whose
    /// own profile must not carry one: the hook collects through that clone,
    /// which must not recurse into the hook or keep itself alive.
    pub(super) fn install_mutation_start<PS, SS>(&mut self, repository: &Repository<PS, SS>)
    where
        PS: BlobGc + 'static,
        SS: MetadataStore + 'static,
    {
        debug_assert!(repository.profile.mutation_start.is_none());
        self.mutation_start = self.pressure_collection_root.as_ref().map(|root| {
            Arc::new(LocalMutationStart::new(repository.clone(), root.clone()))
                as Arc<dyn MutationStart>
        });
    }

    #[cfg(test)]
    pub(super) fn set_emergency_collection(&mut self, enabled: bool) {
        self.emergency_collection = enabled;
    }
}
