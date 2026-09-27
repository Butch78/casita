//! Casitar archive import requests.

use async_trait::async_trait;

use super::{BackendImporter, Importer};
use crate::RootName;
use crate::blob::BlobStore;
use crate::casitar::{
    CasitarImportError, CasitarImportReport, CasitarReader, CasitarRootConflictPolicy,
};
use crate::metadata::MetadataStore;
use crate::repository::Repository;
use tokio::io::AsyncRead;

/// One Casitar closure-archive import request.
pub struct CasitarImport<R> {
    /// Raw archive input or an already-inspected bounded reader.
    input: CasitarInput<R>,
    /// Destination-owned root names in the archive header's canonical order.
    destinations: Vec<RootName>,
    /// Existing-destination policy.
    conflict_policy: CasitarRootConflictPolicy,
}

impl<PS, SS, R> BackendImporter<Repository<PS, SS>> for CasitarImport<R>
where
    PS: BlobStore,
    SS: MetadataStore,
    R: AsyncRead + Unpin + Send,
{
    type Report = CasitarImportReport;
    type Error = CasitarImportError;

    async fn import_into(
        self,
        repository: &Repository<PS, SS>,
    ) -> Result<Self::Report, Self::Error> {
        let reader = match self.input {
            CasitarInput::Stream(input, limits) => CasitarReader::open(input, limits).await?,
            CasitarInput::Parsed(reader) => *reader,
        };
        repository
            .import_casitar(reader, self.destinations, self.conflict_policy)
            .await
    }
}

enum CasitarInput<R> {
    Stream(R, crate::CasitarStreamLimits),
    Parsed(Box<CasitarReader<R>>),
}

impl<R> CasitarImport<R> {
    /// Import an archive under destination names in canonical archive-root order.
    ///
    /// Pass `[name]` for a single-root archive. Import rejects duplicate names
    /// or a count different from the archive's root count before publication.
    pub fn new(input: R, destinations: impl IntoIterator<Item = RootName>) -> Self {
        Self::with_limits(input, destinations, crate::CasitarStreamLimits::default())
    }

    /// Import an unopened archive stream with explicit parsing limits.
    /// Inspected readers retain the limits supplied to `CasitarReader::open`.
    pub fn with_limits(
        input: R,
        destinations: impl IntoIterator<Item = RootName>,
        limits: crate::CasitarStreamLimits,
    ) -> Self {
        Self {
            input: CasitarInput::Stream(input, limits),
            destinations: destinations.into_iter().collect(),
            conflict_policy: CasitarRootConflictPolicy::RequireAbsent,
        }
    }

    /// Choose how imports handle existing destination roots.
    pub fn with_conflict_policy(mut self, policy: CasitarRootConflictPolicy) -> Self {
        self.conflict_policy = policy;
        self
    }

    /// Import a bounded reader whose header has already been inspected.
    /// Existing destinations are rejected unless `with_conflict_policy` is used.
    /// The reader's original limits remain in force; limits cannot be overridden.
    ///
    /// ```compile_fail
    /// use casita::{import::CasitarImport, CasitarStreamLimits, RootName};
    /// use casita::experimental::CasitarReader;
    /// fn override_limits(reader: CasitarReader<&[u8]>, root: RootName) {
    ///     CasitarImport::from_reader(reader, [root]).with_limits(CasitarStreamLimits::default());
    /// }
    /// ```
    #[cfg(feature = "experimental")]
    pub fn from_reader(
        reader: CasitarReader<R>,
        destinations: impl IntoIterator<Item = RootName>,
    ) -> Self {
        Self {
            input: CasitarInput::Parsed(Box::new(reader)),
            destinations: destinations.into_iter().collect(),
            conflict_policy: CasitarRootConflictPolicy::RequireAbsent,
        }
    }
}

#[async_trait]
impl<R: AsyncRead + Unpin + Send> Importer for CasitarImport<R> {
    type Report = CasitarImportReport;
    type Error = crate::Error;

    async fn import(self, repository: &crate::Repository) -> Result<Self::Report, Self::Error> {
        self.import_into(&repository.inner)
            .await
            .map_err(|error| crate::api::Error::classified(error.category(), error))
    }
}

#[cfg(feature = "experimental")]
repository_importer!(
    CasitarImport<R>,
    [R],
    CasitarImportReport,
    CasitarImportError
);
