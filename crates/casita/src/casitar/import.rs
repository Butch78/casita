//! Receiver-verified Casitar import and atomic destination root publication.

use std::collections::BTreeSet;

use tokio::io::AsyncRead;

use super::{CasitarReadFrame, CasitarReader, CasitarStats, CasitarStreamError};
use crate::blob::BlobStore;
use crate::metadata::MetadataStore;
use crate::repository::{ClosureStatus, Repository, RepositoryError, RootExpectation};
use crate::spill::{FrozenSpillSet, SpillSet};
use crate::{
    BlobId, ConditionalPublishResult, ObjectKey, RepositoryErrorCategory, RepositoryRevision,
    RootChange, RootName, SpillMetrics,
};

const MEMBERSHIP_PAGE: usize = 256;

/// Opt-in, exclusive wall-clock phase accounting for the permanent benchmark.
/// No clock reads or per-frame allocations when disabled. Diagnostics never use stdout.
#[derive(Default)]
struct ImportProfile {
    enabled: bool,
    phases: std::collections::BTreeMap<&'static str, (u64, u64)>,
}

impl ImportProfile {
    fn start(&self) -> Option<std::time::Instant> {
        self.enabled.then(std::time::Instant::now)
    }

    fn finish(&mut self, phase: &'static str, start: Option<std::time::Instant>) {
        if let Some(start) = start {
            let entry = self.phases.entry(phase).or_default();
            entry.0 += 1;
            entry.1 = entry
                .1
                .saturating_add(start.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        }
    }

    fn emit(&self) {
        if self.enabled {
            // Phase names are fixed internal identifiers, never input strings.
            let phases = self
                .phases
                .iter()
                .map(|(phase, (calls, nanos))| {
                    format!(r#"{{"phase":"{phase}","calls":{calls},"nanos":{nanos}}}"#)
                })
                .collect::<Vec<_>>()
                .join(",");
            eprintln!("casitar_import_profile {{\"schema_version\":1,\"phases\":[{phases}]}}");
        }
    }
}

/// How an import treats destination names that already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasitarRootConflictPolicy {
    /// Every destination name must be absent before ingestion and at commit.
    RequireAbsent,
    /// Capture each name's current value before ingestion and replace it only
    /// if it remains unchanged through final publication.
    ReplaceIfUnchanged,
}

/// One destination-owned name installed for a canonical archive root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasitarRootMapping {
    /// Canonical root position from the archive header.
    pub index: usize,
    /// Exact immutable archive root.
    pub root: ObjectKey,
    /// Destination-owned retention name.
    pub name: RootName,
}

/// Verified facts and physical/logical progress from a successful import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasitarImportReport {
    /// Canonical archive roots and their destination names.
    pub mappings: Vec<CasitarRootMapping>,
    /// One revision in which every destination root became visible.
    pub destination_revision: RepositoryRevision,
    /// Logical records newly inserted by this attempt.
    pub records_inserted: u64,
    /// Archive records already present as identical immutable state.
    pub records_reused: u64,
    /// Payload frames written because the destination lacked their identity.
    pub payloads_written: u64,
    /// Payload frames verified from the stream and reused physically.
    pub payloads_reused: u64,
    /// Complete structural stream counts and final archive digest.
    pub stats: CasitarStats,
    /// Temporary membership and union-set storage used by the importer.
    pub spill: SpillMetrics,
}

/// Failure while importing an untrusted Casitar stream.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CasitarImportError {
    /// Structural framing, sequence, digest, or stream-limit failure.
    #[error(transparent)]
    Stream(#[from] CasitarStreamError),
    /// Repository storage, namespace verification, state, or traversal failure.
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    /// The caller did not supply exactly one name per canonical header root.
    #[error("Casitar import has {roots} roots but {names} destination names")]
    DestinationCount { roots: usize, names: usize },
    /// Two header roots were mapped to the same mutable destination name.
    #[error("duplicate Casitar destination root name `{0}`")]
    DuplicateDestination(RootName),
    /// Default no-replace policy found an existing destination name.
    #[error("Casitar destination root `{name}` already points to {actual}")]
    DestinationExists { name: RootName, actual: ObjectKey },
    /// A mapped name changed after import preflight and before publication.
    #[error(
        "Casitar destination root `{name}` changed during import: expected {expected:?}, got {actual:?}"
    )]
    DestinationChanged {
        name: RootName,
        expected: Option<ObjectKey>,
        actual: Option<ObjectKey>,
    },
    /// Physical storage finalized a payload under unexpected facts.
    #[error(
        "Casitar payload store finalized {actual} ({actual_size} bytes), expected {expected} ({expected_size} bytes)"
    )]
    PayloadStoreMismatch {
        expected: BlobId,
        expected_size: u64,
        actual: BlobId,
        actual_size: u64,
    },
    /// Namespace verification reproduced a different logical record.
    #[error("Casitar record {key} disagrees with receiver verification")]
    RecordMismatch { key: ObjectKey },
    /// A declared root was not a complete receiver-verified closure.
    #[error("Casitar root {root} is not a complete valid closure: {status:?}")]
    IncompleteRoot {
        root: ObjectKey,
        status: ClosureStatus,
    },
    /// A reachable record was supplied only by ambient destination state.
    #[error("Casitar archive omits reachable record {0}")]
    MissingArchiveRecord(ObjectKey),
    /// An archive record did not belong to any declared root closure.
    #[error("Casitar archive contains unreachable record {0}")]
    UnexpectedArchiveRecord(ObjectKey),
    /// A record named a payload not present as an archive frame.
    #[error("Casitar archive omits referenced payload {0}")]
    MissingArchivePayload(BlobId),
    /// A payload frame was not named by any included record.
    #[error("Casitar archive contains unreferenced payload {0}")]
    UnexpectedArchivePayload(BlobId),
    /// A progress counter exceeded its public `u64` representation.
    #[error("Casitar import progress counter overflow")]
    CountOverflow,
}

impl CasitarImportError {
    /// Stable frontend category for this import failure.
    pub fn category(&self) -> RepositoryErrorCategory {
        use RepositoryErrorCategory as Category;

        match self {
            Self::Stream(error) => error.category(),
            Self::Repository(error) => error.category(),
            Self::DestinationCount { .. } | Self::DuplicateDestination(_) => Category::InvalidInput,
            Self::DestinationExists { .. } | Self::DestinationChanged { .. } => {
                Category::DestinationConflict
            }
            Self::IncompleteRoot { status, .. } => match status {
                ClosureStatus::Unsupported { .. } => Category::Unsupported,
                ClosureStatus::Missing { .. }
                | ClosureStatus::Invalid { .. }
                | ClosureStatus::Complete { .. } => Category::InvalidData,
            },
            Self::PayloadStoreMismatch { .. }
            | Self::RecordMismatch { .. }
            | Self::MissingArchiveRecord(_)
            | Self::UnexpectedArchiveRecord(_)
            | Self::MissingArchivePayload(_)
            | Self::UnexpectedArchivePayload(_)
            | Self::CountOverflow => Category::InvalidData,
        }
    }
}

impl<PS, SS> Repository<PS, SS>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    /// Import one already-opened Casitar stream and atomically name every root.
    ///
    /// `destinations` maps the header's canonical root order one-for-one. The
    /// caller can inspect [`CasitarReader::header`] before this call, so all
    /// names and conflict policy are validated before the first payload write.
    /// One mutation hold excludes collection from that first write through the
    /// final conditional root commit.
    #[tracing::instrument(
        name = "casitar.import",
        skip_all,
        fields(?conflict_policy, destination_count = destinations.len())
    )]
    pub(crate) async fn import_casitar<R>(
        &self,
        mut reader: CasitarReader<R>,
        destinations: Vec<RootName>,
        conflict_policy: CasitarRootConflictPolicy,
    ) -> Result<CasitarImportReport, CasitarImportError>
    where
        R: AsyncRead + Unpin,
    {
        let roots = reader.header().roots().to_vec();
        validate_destinations(self, &roots, &destinations)?;
        let mappings = roots
            .iter()
            .cloned()
            .zip(destinations.iter().cloned())
            .enumerate()
            .map(|(index, (root, name))| CasitarRootMapping { index, root, name })
            .collect::<Vec<_>>();

        let mut profile = ImportProfile {
            enabled: std::env::var_os("CASITA_CASITAR_IMPORT_PROFILE")
                .is_some_and(|value| value == "1"),
            ..Default::default()
        };
        let start = profile.start();
        let mutation = self.mutation_session().await?;
        profile.finish("mutation_start", start);
        mutation
            .write_scope()
            .run(async {
                let start = profile.start();
                let expectations = preflight_roots(self, &mappings, conflict_policy).await?;
                profile.finish("preflight", start);
                let area = self.spill_area();
                let mut archive_records = SpillSet::new(area.clone(), "casitar-import-records");
                let mut archive_payloads = SpillSet::new(area.clone(), "casitar-import-payloads");
                let mut referenced_payloads =
                    SpillSet::new(area.clone(), "casitar-import-referenced-payloads");
                let batch_limit = self.limits().max_batch_objects;
                if batch_limit == 0 {
                    return Err(RepositoryError::LimitExceeded(
                        "Casitar import requires a nonzero mutation batch limit".to_owned(),
                    )
                    .into());
                }
                let batch_limit = batch_limit.min(256);
                let mut pending = Vec::with_capacity(batch_limit);
                let mut records_inserted = 0u64;
                let mut payloads_written = 0u64;
                let mut payloads_reused = 0u64;

                loop {
                    let start = profile.start();
                    let frame = reader.next_frame().await?;
                    profile.finish("frame_decode", start);
                    let Some(frame) = frame else { break };

                    match frame {
                        CasitarReadFrame::Payload { payload, size } => {
                            let start = profile.start();
                            let reused = self
                                .payloads()
                                .has(&payload)
                                .await
                                .map_err(RepositoryError::from)?;
                            profile.finish("payload_lookup", start);
                            let start = profile.start();
                            if reused {
                                reader.read_payload_to(&mut tokio::io::sink()).await?;
                                payloads_reused = checked_increment(payloads_reused)?;
                            } else {
                                let mut writer = self.payloads().open_write().await;
                                reader.read_payload_to(&mut writer).await?;
                                let (actual, actual_size) =
                                    writer.close().await.map_err(RepositoryError::from)?;
                                if actual != payload || actual_size != size {
                                    return Err(CasitarImportError::PayloadStoreMismatch {
                                        expected: payload,
                                        expected_size: size,
                                        actual,
                                        actual_size,
                                    });
                                }
                                payloads_written = checked_increment(payloads_written)?;
                            }
                            profile.finish(
                                if reused {
                                    "payload_reuse_verify"
                                } else {
                                    "payload_write"
                                },
                                start,
                            );
                            let start = profile.start();
                            archive_payloads
                                .insert(payload)
                                .await
                                .map_err(RepositoryError::from)?;
                            profile.finish("payload_membership_insert", start);
                        }
                        CasitarReadFrame::Record(record) => {
                            if archive_records.len() >= self.limits().max_traversal_objects {
                                return Err(RepositoryError::LimitExceeded(format!(
                                    "Casitar import exceeded {} logical records",
                                    self.limits().max_traversal_objects
                                ))
                                .into());
                            }
                            let start = profile.start();
                            archive_records
                                .insert(record.key().clone())
                                .await
                                .map_err(RepositoryError::from)?;
                            referenced_payloads
                                .insert(record.payload())
                                .await
                                .map_err(RepositoryError::from)?;
                            profile.finish("record_membership_insert", start);
                            pending.push(record);
                            if pending.len() == batch_limit {
                                let inserted = publish_records(
                                    &mutation,
                                    pending,
                                    &mut profile,
                                    "publish_batch",
                                )
                                .await?;
                                records_inserted = checked_add_usize(records_inserted, inserted)?;
                                pending = Vec::with_capacity(batch_limit);
                            }
                        }
                    }
                }
                if !pending.is_empty() {
                    let inserted =
                        publish_records(&mutation, pending, &mut profile, "publish_tail").await?;
                    records_inserted = checked_add_usize(records_inserted, inserted)?;
                }
                let (_, stats) = reader.into_inner()?;

                let start = profile.start();
                let archive_records = archive_records
                    .freeze()
                    .await
                    .map_err(RepositoryError::from)?;
                let archive_payloads = archive_payloads
                    .freeze()
                    .await
                    .map_err(RepositoryError::from)?;
                let referenced_payloads = referenced_payloads
                    .freeze()
                    .await
                    .map_err(RepositoryError::from)?;
                validate_payload_membership(&archive_payloads, &referenced_payloads).await?;

                profile.finish("freeze_and_payload_membership", start);
                let start = profile.start();
                let hold = self.retention_hold().await?;
                let mut reachable = SpillSet::new(area.clone(), "casitar-import-reachable");
                for root in &roots {
                    let status = hold.verify_closure_into(root, &mut reachable).await?;
                    if !matches!(status, ClosureStatus::Complete { .. }) {
                        return Err(CasitarImportError::IncompleteRoot {
                            root: root.clone(),
                            status,
                        });
                    }
                }
                profile.finish("verify_closure", start);
                let start = profile.start();
                let reachable = reachable.freeze().await.map_err(RepositoryError::from)?;
                validate_record_membership(&archive_records, &reachable).await?;
                drop(hold);
                profile.finish("record_membership", start);

                let changes = mappings
                    .iter()
                    .map(|mapping| RootChange::Set {
                        name: mapping.name.clone(),
                        target: mapping.root.clone(),
                    })
                    .collect();
                let start = profile.start();
                let destination_revision = match mutation
                    .publish_if_roots_match(Vec::new(), expectations, changes)
                    .await?
                {
                    ConditionalPublishResult::Committed(result) => result.revision,
                    ConditionalPublishResult::RootMismatch {
                        name,
                        expected,
                        actual,
                    } => {
                        return Err(CasitarImportError::DestinationChanged {
                            name,
                            expected,
                            actual,
                        });
                    }
                };

                profile.finish("publish_roots", start);
                profile.emit();
                let records_reused = stats
                    .records
                    .checked_sub(records_inserted)
                    .ok_or(CasitarImportError::CountOverflow)?;
                let report = CasitarImportReport {
                    mappings,
                    destination_revision,
                    records_inserted,
                    records_reused,
                    payloads_written,
                    payloads_reused,
                    stats,
                    spill: area.metrics(),
                };
                tracing::info!(
                    destination_revision = %report.destination_revision,
                    roots = report.mappings.len(),
                    records_inserted = report.records_inserted,
                    records_reused = report.records_reused,
                    payloads_written = report.payloads_written,
                    payloads_reused = report.payloads_reused,
                    archive_bytes = report.stats.archive_bytes,
                    "Casitar import completed"
                );
                Ok(report)
            })
            .await
    }
}

/// Protect each batch in one durable operation, then use the same receiver
/// verification as individual staging. No record becomes visible until every
/// record in this batch agrees with its verified payload.
async fn publish_records<PS: BlobStore, SS: MetadataStore>(
    mutation: &crate::repository::MutationSession<'_, PS, SS>,
    records: Vec<crate::ObjectRecord>,
    profile: &mut ImportProfile,
    phase: &'static str,
) -> Result<usize, CasitarImportError> {
    let start = profile.start();
    mutation.protect_existing_records(&records).await?;
    profile.finish("protect_records", start);
    let mut pending = Vec::with_capacity(records.len());
    for record in records {
        let start = profile.start();
        let staged = mutation
            .stage_existing(record.key().clone(), record.payload())
            .await?;
        profile.finish("stage_existing", start);
        if staged.record() != &record {
            return Err(CasitarImportError::RecordMismatch {
                key: record.key().clone(),
            });
        }
        pending.push(staged);
    }
    let start = profile.start();
    let result = mutation.publish_unrooted(pending).await?;
    profile.finish(phase, start);
    Ok(result.objects_inserted)
}

fn validate_destinations<PS, SS>(
    repository: &Repository<PS, SS>,
    roots: &[ObjectKey],
    destinations: &[RootName],
) -> Result<(), CasitarImportError> {
    if roots.len() != destinations.len() {
        return Err(CasitarImportError::DestinationCount {
            roots: roots.len(),
            names: destinations.len(),
        });
    }
    if destinations.len() > repository.limits().max_root_changes {
        return Err(RepositoryError::LimitExceeded(format!(
            "Casitar import has {} roots, destination limit is {}",
            destinations.len(),
            repository.limits().max_root_changes
        ))
        .into());
    }
    let mut unique = BTreeSet::new();
    for name in destinations {
        if !unique.insert(name) {
            return Err(CasitarImportError::DuplicateDestination(name.clone()));
        }
    }
    Ok(())
}

async fn preflight_roots<PS, SS>(
    repository: &Repository<PS, SS>,
    mappings: &[CasitarRootMapping],
    policy: CasitarRootConflictPolicy,
) -> Result<Vec<RootExpectation>, CasitarImportError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let snapshot = repository
        .metadata()
        .snapshot()
        .await
        .map_err(RepositoryError::from)?;
    let mut expectations = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let actual = snapshot
            .root(&mapping.name)
            .await
            .map_err(RepositoryError::from)?;
        if matches!(policy, CasitarRootConflictPolicy::RequireAbsent)
            && let Some(actual) = actual
        {
            return Err(CasitarImportError::DestinationExists {
                name: mapping.name.clone(),
                actual,
            });
        }
        expectations.push(RootExpectation {
            name: mapping.name.clone(),
            target: match policy {
                CasitarRootConflictPolicy::RequireAbsent => None,
                CasitarRootConflictPolicy::ReplaceIfUnchanged => actual,
            },
        });
    }
    Ok(expectations)
}

async fn validate_record_membership(
    archive: &FrozenSpillSet<ObjectKey>,
    reachable: &FrozenSpillSet<ObjectKey>,
) -> Result<(), CasitarImportError> {
    let mut after = None;
    loop {
        let page = reachable
            .page(after, MEMBERSHIP_PAGE)
            .await
            .map_err(RepositoryError::from)?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        for key in page {
            if !archive
                .contains(&key)
                .await
                .map_err(RepositoryError::from)?
            {
                return Err(CasitarImportError::MissingArchiveRecord(key));
            }
        }
        after = Some(last);
    }
    let mut after = None;
    loop {
        let page = archive
            .page(after, MEMBERSHIP_PAGE)
            .await
            .map_err(RepositoryError::from)?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        for key in page {
            if !reachable
                .contains(&key)
                .await
                .map_err(RepositoryError::from)?
            {
                return Err(CasitarImportError::UnexpectedArchiveRecord(key));
            }
        }
        after = Some(last);
    }
    Ok(())
}

async fn validate_payload_membership(
    archive: &FrozenSpillSet<BlobId>,
    referenced: &FrozenSpillSet<BlobId>,
) -> Result<(), CasitarImportError> {
    let mut after = None;
    loop {
        let page = referenced
            .page(after, MEMBERSHIP_PAGE)
            .await
            .map_err(RepositoryError::from)?;
        let Some(last) = page.last().copied() else {
            break;
        };
        for payload in page {
            if !archive
                .contains(&payload)
                .await
                .map_err(RepositoryError::from)?
            {
                return Err(CasitarImportError::MissingArchivePayload(payload));
            }
        }
        after = Some(last);
    }
    let mut after = None;
    loop {
        let page = archive
            .page(after, MEMBERSHIP_PAGE)
            .await
            .map_err(RepositoryError::from)?;
        let Some(last) = page.last().copied() else {
            break;
        };
        for payload in page {
            if !referenced
                .contains(&payload)
                .await
                .map_err(RepositoryError::from)?
            {
                return Err(CasitarImportError::UnexpectedArchivePayload(payload));
            }
        }
        after = Some(last);
    }
    Ok(())
}

fn checked_increment(value: u64) -> Result<u64, CasitarImportError> {
    value
        .checked_add(1)
        .ok_or(CasitarImportError::CountOverflow)
}

fn checked_add_usize(value: u64, increment: usize) -> Result<u64, CasitarImportError> {
    value
        .checked_add(u64::try_from(increment).map_err(|_| CasitarImportError::CountOverflow)?)
        .ok_or(CasitarImportError::CountOverflow)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    use super::*;
    use crate::{
        CasitarExportTarget, CasitarHeader, CasitarStreamLimits, CasitarWriter, Directory,
        MemoryBlobStore, MemoryMetadataStore, Node, ObjectRecord, PathComponent, RootChange,
        SpillLimits,
    };

    #[derive(Debug, Default)]
    struct VecWriter(Vec<u8>);

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.extend_from_slice(buffer);
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct BlockingAfterPrefix {
        prefix: std::io::Cursor<Vec<u8>>,
        blocked: Arc<AtomicBool>,
    }

    impl AsyncRead for BlockingAfterPrefix {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let position = self.prefix.position() as usize;
            let bytes = self.prefix.get_ref();
            if position == bytes.len() {
                self.blocked.store(true, Ordering::SeqCst);
                return Poll::Pending;
            }
            let count = buffer.remaining().min(bytes.len() - position);
            buffer.put_slice(&bytes[position..position + count]);
            self.prefix.set_position((position + count) as u64);
            Poll::Ready(Ok(()))
        }
    }

    async fn graph_archive() -> (Vec<u8>, Vec<ObjectKey>) {
        let source = Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new().unwrap());
        let mutation = source.mutation_session().await.unwrap();
        let child_bytes = b"shared imported child";
        let child = mutation.stage_blob(child_bytes).await.unwrap();
        let child_payload = child.record().payload();
        let first_directory = Directory::try_from_iter([(
            PathComponent::try_from("first").unwrap(),
            Node::File {
                digest: child_payload,
                size: child_bytes.len() as u64,
                executable: false,
            },
        )])
        .unwrap();
        let second_directory = Directory::try_from_iter([(
            PathComponent::try_from("second").unwrap(),
            Node::File {
                digest: child_payload,
                size: child_bytes.len() as u64,
                executable: false,
            },
        )])
        .unwrap();
        let first = mutation.stage_directory(&first_directory).await.unwrap();
        let second = mutation.stage_directory(&second_directory).await.unwrap();
        let mut roots = vec![first.record().key().clone(), second.record().key().clone()];
        mutation
            .publish_unrooted(vec![child, first, second])
            .await
            .unwrap();
        drop(mutation);
        roots.sort();

        let (writer, _) = source
            .export_casitar(
                roots.iter().cloned().map(CasitarExportTarget::ExactObject),
                VecWriter::default(),
                CasitarStreamLimits::default(),
            )
            .await
            .unwrap();
        (writer.0, roots)
    }

    #[tokio::test]
    async fn a_forged_record_prevents_the_entire_protected_batch_from_publication() {
        let repository = Repository::memory().unwrap();
        let mutation = repository.mutation_session().await.unwrap();
        let first = mutation.stage_blob(b"valid batch member").await.unwrap();
        let second = mutation.stage_blob(b"forged batch member").await.unwrap();
        let first_record = first.record().clone();
        let second_record = second.record();
        let forged = ObjectRecord::new(
            second_record.key().clone(),
            second_record.payload(),
            second_record.payload_size() + 1,
            vec![],
        )
        .unwrap();
        let result = publish_records(
            &mutation,
            vec![first_record.clone(), forged],
            &mut ImportProfile::default(),
            "publish_tail",
        )
        .await;
        assert!(matches!(
            result,
            Err(CasitarImportError::RecordMismatch { .. })
        ));
        let snapshot = repository.metadata().snapshot().await.unwrap();
        assert!(snapshot.object(first_record.key()).await.unwrap().is_none());
        assert!(
            snapshot
                .object(second_record.key())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn imports_exact_multi_root_closure_atomically_and_reuses_on_retry() {
        let (archive, roots) = graph_archive().await;
        let destination = Repository::memory()
            .unwrap()
            .with_spill_limits(SpillLimits {
                max_memory_objects: 1,
                max_spill_bytes: 64 * 1024 * 1024,
            });
        let names = vec![
            RootName::try_from("imports/first").unwrap(),
            RootName::try_from("imports/second").unwrap(),
        ];
        let reader = CasitarReader::open(
            std::io::Cursor::new(archive.clone()),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        let report = destination
            .import_casitar(
                reader,
                names.clone(),
                CasitarRootConflictPolicy::RequireAbsent,
            )
            .await
            .unwrap();

        assert_eq!(report.records_inserted, 3);
        assert_eq!(report.records_reused, 0);
        assert_eq!(report.payloads_written, 3);
        assert_eq!(
            report.stats.archive_digest,
            Some(crate::Digest::hash(&archive))
        );
        assert!(report.spill.files_opened > 0);
        let snapshot = destination.metadata().snapshot().await.unwrap();
        assert_eq!(snapshot.revision(), report.destination_revision);
        for ((name, root), mapping) in names.iter().zip(&roots).zip(&report.mappings) {
            assert_eq!(snapshot.root(name).await.unwrap().as_ref(), Some(root));
            assert_eq!(&mapping.name, name);
            assert_eq!(&mapping.root, root);
        }

        let retry_names = vec![
            RootName::try_from("retry/first").unwrap(),
            RootName::try_from("retry/second").unwrap(),
        ];
        let reader = CasitarReader::open(
            std::io::Cursor::new(archive),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        let retry = destination
            .import_casitar(
                reader,
                retry_names,
                CasitarRootConflictPolicy::RequireAbsent,
            )
            .await
            .unwrap();
        assert_eq!(retry.records_inserted, 0);
        assert_eq!(retry.records_reused, 3);
        assert_eq!(retry.payloads_written, 0);
        assert_eq!(retry.payloads_reused, 3);
    }

    #[tokio::test]
    async fn ambient_destination_objects_cannot_conceal_an_omitted_record() {
        let child_bytes = b"ambient child";
        let child_payload = BlobId::new(crate::Digest::hash(child_bytes));
        let directory = Directory::try_from_iter([(
            PathComponent::try_from("child").unwrap(),
            Node::File {
                digest: child_payload,
                size: child_bytes.len() as u64,
                executable: false,
            },
        )])
        .unwrap();
        let directory_bytes = directory.encode();
        let directory_payload = BlobId::new(crate::Digest::hash(&directory_bytes));
        let directory_key = ObjectKey::directory(directory.digest());
        let directory_record = ObjectRecord::new(
            directory_key.clone(),
            directory_payload,
            directory_bytes.len() as u64,
            vec![ObjectKey::blob(child_payload)],
        )
        .unwrap();

        let header = CasitarHeader::new(vec![directory_key.clone()]).unwrap();
        let mut writer =
            CasitarWriter::new(VecWriter::default(), header, CasitarStreamLimits::default())
                .await
                .unwrap();
        let mut source = std::io::Cursor::new(directory_bytes);
        writer
            .write_payload(
                directory_payload,
                directory_record.payload_size(),
                &mut source,
            )
            .await
            .unwrap();
        writer.write_record(&directory_record).await.unwrap();
        let (archive, _) = writer.finish().await.unwrap();

        let destination = Repository::memory().unwrap();
        let mutation = destination.mutation_session().await.unwrap();
        let child = mutation.stage_blob(child_bytes).await.unwrap();
        mutation.publish_unrooted(vec![child]).await.unwrap();
        drop(mutation);
        let name = RootName::try_from("imports/incomplete").unwrap();
        let reader = CasitarReader::open(
            std::io::Cursor::new(archive.0),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        let error = destination
            .import_casitar(
                reader,
                vec![name.clone()],
                CasitarRootConflictPolicy::RequireAbsent,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CasitarImportError::MissingArchiveRecord(key) if key == ObjectKey::blob(child_payload)
        ));
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn unreachable_records_and_payloads_publish_no_root() {
        let mut entries = vec![(b"selected".as_slice()), (b"unrelated".as_slice())]
            .into_iter()
            .map(|body| {
                let payload = BlobId::new(crate::Digest::hash(body));
                let record =
                    ObjectRecord::new(ObjectKey::blob(payload), payload, body.len() as u64, vec![])
                        .unwrap();
                (payload, body, record)
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(payload, _, _)| *payload);
        let selected = entries[0].2.key().clone();
        let header = CasitarHeader::new(vec![selected]).unwrap();
        let mut writer =
            CasitarWriter::new(VecWriter::default(), header, CasitarStreamLimits::default())
                .await
                .unwrap();
        for (payload, body, _) in &entries {
            let mut source = std::io::Cursor::new(*body);
            writer
                .write_payload(*payload, body.len() as u64, &mut source)
                .await
                .unwrap();
        }
        for (_, _, record) in &entries {
            writer.write_record(record).await.unwrap();
        }
        let (archive, _) = writer.finish().await.unwrap();

        let destination = Repository::memory().unwrap();
        let name = RootName::try_from("imports/extra").unwrap();
        let reader = CasitarReader::open(
            std::io::Cursor::new(archive.0),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        let error = destination
            .import_casitar(
                reader,
                vec![name.clone()],
                CasitarRootConflictPolicy::RequireAbsent,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CasitarImportError::UnexpectedArchiveRecord(_)
        ));
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn root_conflicts_are_preflighted_and_replacement_is_conditional() {
        let (archive, roots) = graph_archive().await;
        let destination = Repository::memory().unwrap();
        let name = RootName::try_from("imports/current").unwrap();
        let mutation = destination.mutation_session().await.unwrap();
        let old = mutation.stage_blob(b"old root").await.unwrap();
        let old_key = old.record().key().clone();
        mutation
            .publish(
                vec![old],
                vec![RootChange::Set {
                    name: name.clone(),
                    target: old_key.clone(),
                }],
            )
            .await
            .unwrap();
        drop(mutation);

        let reader = CasitarReader::open(
            std::io::Cursor::new(archive.clone()),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        let error = destination
            .import_casitar(
                reader,
                vec![name.clone(), RootName::try_from("imports/other").unwrap()],
                CasitarRootConflictPolicy::RequireAbsent,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CasitarImportError::DestinationExists { actual, .. } if actual == old_key
        ));
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&roots[0])
                .await
                .unwrap()
                .is_none()
        );

        let second_name = RootName::try_from("imports/other").unwrap();
        let reader = CasitarReader::open(
            std::io::Cursor::new(archive),
            CasitarStreamLimits::default(),
        )
        .await
        .unwrap();
        destination
            .import_casitar(
                reader,
                vec![name.clone(), second_name.clone()],
                CasitarRootConflictPolicy::ReplaceIfUnchanged,
            )
            .await
            .unwrap();
        let snapshot = destination.metadata().snapshot().await.unwrap();
        assert_eq!(snapshot.root(&name).await.unwrap(), Some(roots[0].clone()));
        assert_eq!(
            snapshot.root(&second_name).await.unwrap(),
            Some(roots[1].clone())
        );
    }

    #[tokio::test]
    async fn import_admits_collection_before_final_root_publication() {
        let (_, roots) = graph_archive().await;
        let blocked = Arc::new(AtomicBool::new(false));
        let input = BlockingAfterPrefix {
            prefix: std::io::Cursor::new(CasitarHeader::new(roots).unwrap().encode()),
            blocked: blocked.clone(),
        };
        let reader = CasitarReader::open(input, CasitarStreamLimits::default())
            .await
            .unwrap();
        let destination = Arc::new(Repository::memory().unwrap());
        let name = RootName::try_from("imports/blocked").unwrap();
        let task_destination = destination.clone();
        let task_name = name.clone();
        let task = tokio::spawn(async move {
            task_destination
                .import_casitar(
                    reader,
                    vec![
                        task_name,
                        RootName::try_from("imports/blocked-two").unwrap(),
                    ],
                    CasitarRootConflictPolicy::RequireAbsent,
                )
                .await
        });

        while !blocked.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            destination
                .try_collect()
                .await
                .unwrap()
                .removed
                .logical_objects,
            0
        );

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap()
                .is_none()
        );
        destination.try_collect().await.unwrap();
    }
}
