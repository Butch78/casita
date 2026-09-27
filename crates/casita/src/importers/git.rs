//! Native Git object-database import requests.

use async_trait::async_trait;

use super::{BackendImporter, Importer};
use crate::blob::BlobStore;
use crate::git::repository::{GitViewError, NativeGitImportOptions, NativeGitImportOutcome};
use crate::metadata::MetadataStore;
use crate::repository::Repository;
use std::path::PathBuf;

/// One native Git object-database import request.
#[derive(Debug, Clone)]
pub struct GitImport {
    /// Local working tree or bare Git repository.
    source: PathBuf,
    /// Selected refs and destination view name.
    options: NativeGitImportOptions,
}

impl<PS, SS> BackendImporter<Repository<PS, SS>> for GitImport
where
    PS: BlobStore,
    SS: MetadataStore,
{
    type Report = NativeGitImportOutcome;
    type Error = GitViewError;

    async fn import_into(
        self,
        repository: &Repository<PS, SS>,
    ) -> Result<Self::Report, Self::Error> {
        repository
            .import_native_git_view(self.source, &self.options)
            .await
    }
}

impl GitImport {
    /// Import local branches and tags under `git/<view>`.
    pub fn new(source: impl Into<PathBuf>, view: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            options: NativeGitImportOptions {
                view_name: view.into(),
                ..Default::default()
            },
        }
    }

    /// Select exact full ref names. Empty selects all local branches and tags.
    ///
    /// Invalid names return [`crate::ErrorKind::InvalidInput`]. Each call replaces
    /// the previous selection.
    pub fn with_refs(
        mut self,
        refs: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, crate::Error> {
        self.options.refs = refs
            .into_iter()
            .map(|name| crate::CanonicalRefName::try_from(name.as_ref()))
            .collect::<Result<_, _>>()
            .map_err(|error| crate::Error::classified(crate::ErrorKind::InvalidInput, error))?;
        Ok(self)
    }

    /// Retain exact full hexadecimal object IDs under `refs/casita/pins/<oid>`.
    /// Each call replaces the previous pinned selection.
    pub fn with_revisions(mut self, revisions: Vec<String>) -> Result<Self, crate::Error> {
        for revision in &revisions {
            if !matches!(revision.len(), 40 | 64)
                || !revision.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(crate::Error::classified(
                    crate::ErrorKind::InvalidInput,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "revisions require full hexadecimal Git object IDs",
                    ),
                ));
            }
        }
        self.options.revisions = revisions;
        Ok(self)
    }

    /// Bound the size of a source pack retained as a full-clone cache.
    ///
    /// Zero disables retention. A pack is cached only when it exactly matches
    /// the selected view; this limit does not bound import memory usage.
    pub fn with_max_cached_pack_bytes(mut self, limit: u64) -> Self {
        self.options.max_cached_pack_bytes = limit;
        self
    }

    /// Bound concurrently staged Git objects. One selects serial staging.
    pub fn with_concurrency(mut self, limit: std::num::NonZeroUsize) -> Self {
        self.options.concurrency = limit;
        self
    }

    /// Bound decoded source bytes held by staging futures. A larger object runs
    /// alone; Gix caches and payload-store buffers are outside this budget.
    pub fn with_max_buffered_bytes(mut self, limit: std::num::NonZeroU64) -> Self {
        self.options.max_buffered_bytes = limit;
        self
    }
}

#[async_trait]
impl Importer for GitImport {
    type Report = NativeGitImportOutcome;
    type Error = crate::Error;

    async fn import(self, repository: &crate::Repository) -> Result<Self::Report, Self::Error> {
        self.import_into(&repository.inner)
            .await
            .map_err(|error| crate::api::Error::classified(error.category(), error))
    }
}

#[cfg(feature = "experimental")]
repository_importer!(GitImport, [], NativeGitImportOutcome, GitViewError);
