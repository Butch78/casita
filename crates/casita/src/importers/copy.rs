//! Retained graph copy requests.

use async_trait::async_trait;

use super::{BackendImporter, Importer};
use crate::blob::BlobStore;
use crate::metadata::MetadataStore;
use crate::repository::Repository;
use crate::sync::{
    DestinationRoot, ObjectRequest, TransferError, TransferRequest, TransferSelection,
    TransferSource,
};
use crate::{ObjectKey, RootName};

/// Copy a named source graph and atomically create or replace its destination root.
/// The source name is resolved when import starts, within the same retained
/// snapshot used to verify and copy its graph.
pub struct CopyImport<'source> {
    source: &'source dyn TransferSource,
    source_name: RootName,
    destination_name: RootName,
}

impl<'source> CopyImport<'source> {
    /// Copy from a built-in repository, retaining the source throughout transfer.
    pub fn new(
        source: &'source crate::Repository,
        source_name: RootName,
        destination_name: RootName,
    ) -> Self {
        Self {
            source: &source.inner,
            source_name,
            destination_name,
        }
    }

    /// Copy from an experimental repository or custom retained transfer source.
    #[cfg(feature = "experimental")]
    pub fn from_source(
        source: &'source dyn TransferSource,
        source_name: RootName,
        destination_name: RootName,
    ) -> Self {
        Self {
            source,
            source_name,
            destination_name,
        }
    }
}

impl<PS: BlobStore, SS: MetadataStore> BackendImporter<Repository<PS, SS>> for CopyImport<'_> {
    type Report = ObjectKey;
    type Error = TransferError;

    async fn import_into(
        self,
        repository: &Repository<PS, SS>,
    ) -> Result<ObjectKey, TransferError> {
        let source = self
            .source
            .begin_transfer(TransferSelection::Selected {
                objects: Vec::new(),
                roots: vec![self.source_name.clone()],
            })
            .await?;
        let key = source
            .root(&self.source_name)
            .await?
            .ok_or(TransferError::MissingSourceRoot(self.source_name))?;
        crate::sync::transfer(
            &crate::sync::HeldSession(source.as_ref()),
            repository,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: key.clone(),
                    recursive: true,
                }],
                roots: vec![DestinationRoot {
                    name: self.destination_name,
                    target: key.clone(),
                }],
            },
            crate::sync::TransferOptions::default(),
        )
        .await?;
        Ok(key)
    }
}

#[async_trait]
impl<'source> Importer for CopyImport<'source> {
    type Report = ObjectKey;
    type Error = crate::Error;

    async fn import(self, repository: &crate::Repository) -> Result<ObjectKey, crate::Error> {
        self.import_into(&repository.inner)
            .await
            .map_err(|error| crate::api::Error::classified(error.category(), error))
    }
}

#[cfg(feature = "experimental")]
#[async_trait]
impl<'source, PS: BlobStore, SS: MetadataStore> Importer<Repository<PS, SS>>
    for CopyImport<'source>
{
    type Report = ObjectKey;
    type Error = TransferError;

    async fn import(self, repository: &Repository<PS, SS>) -> Result<ObjectKey, TransferError> {
        self.import_into(repository).await
    }
}
