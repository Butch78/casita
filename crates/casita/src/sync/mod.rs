//! Repository synchronization through verified object and payload transfer.
//!
//! Discovery trusts sender records only as traversal hints. Every new payload
//! is verified again by the destination's registered object format before its
//! record is committed. Requested roots are one final atomic mutation.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use futures::stream::{self, StreamExt, TryStreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::BlobId;
use crate::ChunkMeta;
use crate::blob::{BlobChunkSource, BlobStore};
use crate::directory::Directory;
use crate::format::FormatError;
use crate::metadata::{MetadataError, MetadataStore};
use crate::node::Node;
use crate::object::{ObjectKey, ObjectRecord, RepositoryRevision, RootName};
use crate::path::PathComponent;
use crate::repository::{ClosureStatus, MutationSession, Repository, RepositoryError};
use crate::spill::{SpillSet, TraversalQueue};
use crate::sync::sliced::{SliceSources, SliceStats};

pub mod sliced;
#[cfg(feature = "ssh")]
pub(crate) mod ssh;
pub mod wire;

/// Maximum number of selected objects in one transfer request.
pub const MAX_TRANSFER_REQUEST_OBJECTS: usize = 4_096;
/// Maximum canonical request footprint before discovery begins.
pub const MAX_TRANSFER_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum object-record lookups sent through one source-session batch.
pub const MAX_TRANSFER_DISCOVERY_BATCH_OBJECTS: usize = 256;
/// Maximum simultaneous payload pipelines admitted by logical discovery.
/// Payloads one batch request may carry, matching the bound sources apply.
pub(crate) const MAX_PAYLOAD_BATCH_OBJECTS: usize = 256;
/// Plaintext one batch request may carry. Both ends hold a whole batch in
/// memory, and a round is sized to one, so this is what a round trip buys.
/// Measured on a rebuilt closure over 20 Mbit at 50 ms, the rebuild costs 227
/// requests and 62 s at 4 MiB, 168 and 58 s at 8, 133 and 55 s at 16, and 114
/// and 55 s at 32: the curve flattens once round trips stop being the bound,
/// so the larger buffer past here buys nothing.
pub(crate) const MAX_PAYLOAD_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONCURRENT_PAYLOAD_TRANSFERS: usize = 16;
/// Maximum simultaneous physical reads across every payload in one transfer.
const MAX_CONCURRENT_PHYSICAL_TRANSFERS: usize = 16;

/// One explicitly selected object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRequest {
    /// Exact namespace-qualified key.
    pub key: ObjectKey,
    /// Whether to discover and transfer the complete stored forward closure.
    pub recursive: bool,
}

/// One destination root to publish after every transfer batch succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationRoot {
    /// Exact destination-owned root name.
    pub name: RootName,
    /// Complete locally verified target required at final commit.
    pub target: ObjectKey,
}

/// Bounded logical transfer request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferRequest {
    /// Selected objects, in caller reporting order.
    pub objects: Vec<ObjectRequest>,
    /// Destination roots published together at the end.
    pub roots: Vec<DestinationRoot>,
}

/// How recursive transfer treats a matching, verified destination closure.
/// This is receiver policy and does not change the frozen request encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransferDiscovery {
    /// Discover the entire requested source closure, including descendants
    /// already present at the destination. Missing source records still fail.
    #[default]
    Exhaustive,
    /// Stop below matching objects whose destination closure is verified.
    /// Descendants below such objects are neither fetched nor audited at the
    /// source. Incomplete destination closures are always discovered normally.
    ReuseVerified,
}

impl TransferRequest {
    /// Encode the frozen v0.2 transfer-request wire form.
    pub fn encode(&self) -> Vec<u8> {
        wire::encode_request(self)
    }

    /// Decode one bounded frozen v0.2 transfer request.
    pub fn decode(encoded: &[u8]) -> Result<Self, wire::TransferWireError> {
        wire::decode_request(encoded)
    }
}

/// Final closure state of one requested object at the destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedStatus {
    /// The selected record itself is absent.
    Missing(ObjectKey),
    /// The record exists but a reachable boundary is absent.
    Incomplete(ObjectKey),
    /// The record, payload, relation, or namespace is invalid/unavailable.
    Invalid(ObjectKey),
    /// The complete destination closure verifies.
    Complete(ObjectKey),
}

/// Durable progress after a successful transfer operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    /// Destination revision after the last logical commit.
    pub destination_revision: RepositoryRevision,
    /// Records newly inserted into destination state.
    pub published_objects: u64,
    /// Whole payloads or manifests made newly present.
    pub payloads_sent: u64,
    /// Payloads already present and reused.
    pub payloads_reused: u64,
    /// Physical chunks sent through compatible chunk negotiation.
    pub chunks_sent: u64,
    /// Physical chunks already present at the destination.
    pub chunks_reused: u64,
    /// Plaintext bytes of sliced payloads resolved from blobs the
    /// destination already held.
    pub slice_copy_bytes: u64,
    /// Plaintext bytes of sliced payloads carried as literal segments.
    pub slice_literal_bytes: u64,
    /// Statuses corresponding exactly to [`TransferRequest::objects`].
    pub requested: Vec<RequestedStatus>,
}

impl TransferProgress {
    /// Encode the frozen v0.2 progress/final-result wire form.
    pub fn encode(&self) -> Vec<u8> {
        wire::encode_progress(self)
    }

    /// Decode one bounded frozen v0.2 progress/final-result value.
    pub fn decode(encoded: &[u8]) -> Result<Self, wire::TransferWireError> {
        wire::decode_progress(encoded)
    }
}

/// Completed transfer result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferResult {
    /// Durable receiver progress and selected-graph statuses.
    pub progress: TransferProgress,
}

/// Result of resolving and transferring one filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTransferResult {
    /// Node authenticated by the source root and verified directory spine.
    /// `None` is an authenticated absent path.
    pub node: Option<Node>,
    /// Closure transfer result. Absent paths and inline symlinks have no
    /// standalone closure and therefore return `None` here.
    pub transfer: Option<TransferResult>,
}

/// One immutable directory record and its canonical payload in a path proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProofEntry {
    /// Record advertised for this directory key.
    pub record: ObjectRecord,
    /// Complete canonical directory payload authenticated by the record.
    pub payload: Vec<u8>,
}

/// Atomic proof of one path beneath a named root at an exact source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProof {
    /// Revision retained while the proof was assembled.
    pub revision: RepositoryRevision,
    /// Named root requested by the receiver.
    pub root_name: RootName,
    /// Directory key selected by the requested named root.
    pub root: ObjectKey,
    /// Ordered directory spine, beginning with [`Self::root`].
    pub directories: Vec<PathProofEntry>,
}

/// Optional result of a capable source's atomic path-proof operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathProofResponse {
    /// This session cannot resolve a path atomically; use ordinary reads.
    Unsupported,
    /// The named root is absent from the retained source snapshot.
    MissingRoot,
    /// The returned proof must be independently verified by the receiver.
    Proof(PathProof),
}

/// Transfer failure domains.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// Request item count or encoded footprint exceeds the frozen bound.
    #[error("invalid transfer request: {0}")]
    InvalidRequest(String),
    /// The sender snapshot lacks an exact record or its complete payload.
    #[error("transfer source is incomplete at {key}")]
    IncompleteSource {
        /// First deterministic missing boundary.
        key: ObjectKey,
    },
    /// The sender's logical-state snapshot failed.
    #[error("transfer source state failed: {0}")]
    SourceMetadata(#[source] MetadataError),
    /// Reading sender physical payload data failed.
    #[error("transfer source payload for {key} failed: {error}")]
    SourcePayload {
        /// Object whose payload was being copied.
        key: ObjectKey,
        /// Backend failure.
        #[source]
        error: crate::error::Error,
    },
    /// A source payload failed deterministic object-format verification.
    #[error("transfer source object {key} failed format verification: {error}")]
    SourceFormat {
        /// Exact source object being verified.
        key: ObjectKey,
        /// Deterministic verification failure.
        #[source]
        error: FormatError,
    },
    /// A named root was absent from the stable source snapshot.
    #[error("transfer source root `{0}` is absent")]
    MissingSourceRoot(RootName),
    /// A path proof or selected node disagreed with verified source data.
    #[error("transfer source path selection at {key} is invalid: {message}")]
    InvalidSourceSelection {
        /// Object at the failing path boundary.
        key: ObjectKey,
        /// Deterministic mismatch description.
        message: String,
    },
    /// An authenticated source transport failed or violated its bounded wire
    /// contract.
    #[error("transfer source transport failed: {0}")]
    SourceTransport(String),
    /// Sender and receiver claim structurally different immutable records for
    /// one exact key.
    #[error("destination has a different immutable record for {0}")]
    ImmutableConflict(ObjectKey),
    /// Destination staging, verification, mutation, or storage failed.
    #[error("transfer destination failed: {0}")]
    Destination(#[source] RepositoryError),
}

impl TransferError {
    /// Stable failure category shared by application and transport adapters.
    pub fn category(&self) -> crate::RepositoryErrorCategory {
        use crate::RepositoryErrorCategory as Category;
        match self {
            Self::InvalidRequest(_) => Category::InvalidInput,
            Self::IncompleteSource { .. } | Self::MissingSourceRoot(_) => Category::Absent,
            Self::SourcePayload { .. } | Self::SourceTransport(_) | Self::SourceMetadata(_) => {
                Category::Backend
            }
            Self::SourceFormat { .. } | Self::InvalidSourceSelection { .. } => {
                Category::InvalidData
            }
            Self::ImmutableConflict(_) => Category::ImmutableConflict,
            Self::Destination(error) => error.category(),
        }
    }
}

impl From<RepositoryError> for TransferError {
    fn from(error: RepositoryError) -> Self {
        Self::Destination(error)
    }
}

/// Sequential payload reader supplied by a stable transfer source session.
pub type TransferPayloadReader = Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>;

/// One batched payload already decoded into plaintext, with the slice
/// accounting of the frame it arrived in.
pub type TransferBatchPayload = (TransferPayloadReader, SliceStats);

/// Ordered metadata and optional plaintext fetched in one transport exchange.
pub struct TransferObjectBatch {
    /// One answer per requested key, preserving duplicates and absent records.
    pub records: Vec<Option<ObjectRecord>>,
    /// One decoded payload per record when the entire batch fits the
    /// requested bound; an entry is `None` when its frame named a source the
    /// receiver lacks, so that payload streams individually. `None` overall
    /// keeps ordinary payload streaming available.
    pub payloads: Option<Vec<Option<TransferBatchPayload>>>,
}

/// Records and payloads a source volunteered for one discovery request:
/// the requested keys' records, records of their descendants that fit the
/// answer bound, and decoded payloads for a prefix of those the receiver is
/// likely to lack. Everything is untrusted until the receiver verifies it.
pub struct TransferDiscoveryAnswer {
    /// Records in the source's breadth-first order, requested keys first.
    pub records: Vec<ObjectRecord>,
    /// Decoded payloads by key; `None` marks a frame that named a source the
    /// receiver lacks and was consumed.
    pub payloads: Vec<(ObjectKey, Option<TransferBatchPayload>)>,
}

/// One retained logical record and its optional physical chunk map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectChunks {
    /// Immutable record from the session's bound snapshot.
    pub record: ObjectRecord,
    /// `None` means this source cannot supply a physical map for the record.
    pub chunks: Option<Vec<ChunkMeta>>,
}

/// Stable source session. The graphs selected during acquisition and their
/// physical data remain protected while the session lives; arbitrary reads
/// require a snapshot-wide selection. Keep the session alive through its readers.
///
/// This contract deliberately exposes transfer-shaped reads instead of a
/// source repository's state and payload backends. Local repositories and
/// authenticated transports therefore use the exact same receiver-side
/// traversal, verification, mutation, and root logic.
#[async_trait]
#[auto_impl::auto_impl(&)]
pub trait TransferReadSession: Send + Sync {
    /// Exact revision bound for the lifetime of this source session.
    fn revision(&self) -> RepositoryRevision;

    /// Look up one immutable object record in the bound source snapshot.
    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError>;

    /// Look up immutable object records in input order, preserving duplicates.
    ///
    /// The default preserves compatibility with simple local adapters. Remote
    /// sessions should override this to avoid one network round trip per
    /// object.
    async fn objects(
        &self,
        keys: &[ObjectKey],
    ) -> Result<Vec<Option<ObjectRecord>>, TransferError> {
        let mut records = Vec::with_capacity(keys.len());
        for key in keys {
            records.push(self.object(key).await?);
        }
        Ok(records)
    }

    /// Read records and physical chunk maps together, in key order.
    ///
    /// Missing records are `None`; present records can have an unsupported
    /// (`None`) chunk map. Duplicates are preserved. Remote implementations can
    /// use one bounded RPC instead of separate metadata and map round trips.
    /// The default composes `objects` and `chunks_batch`, including for split
    /// sources whose records and physical chunks come from different sessions.
    async fn objects_with_chunks(
        &self,
        keys: &[ObjectKey],
    ) -> Result<Vec<Option<ObjectChunks>>, TransferError> {
        let records = self.objects(keys).await?;
        if records.len() != keys.len()
            || records
                .iter()
                .zip(keys)
                .any(|(record, key)| record.as_ref().is_some_and(|record| record.key() != key))
        {
            return Err(TransferError::SourceTransport(
                "invalid object batch".into(),
            ));
        }
        let present: Vec<_> = records.iter().flatten().cloned().collect();
        let maps = self.chunks_batch(&present).await?;
        if maps.len() != present.len() {
            return Err(TransferError::SourceTransport(
                "invalid chunk map batch".into(),
            ));
        }
        let mut maps = maps.into_iter();
        Ok(records
            .into_iter()
            .map(|record| {
                record.map(|record| ObjectChunks {
                    record,
                    chunks: maps.next().expect("validated map count"),
                })
            })
            .collect())
    }

    /// Look up one named root in the bound source snapshot.
    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError>;

    /// Resolve a named-root path in one source operation when supported.
    ///
    /// Implementations supply untrusted proof bytes. The transfer engine
    /// verifies the retained revision, root binding, every directory payload,
    /// every parent link, and the exact path before using the result. The
    /// default keeps local and object-only transports on the ordinary spine
    /// walk.
    async fn path_proof(
        &self,
        _root: &RootName,
        _components: &[PathComponent],
    ) -> Result<PathProofResponse, TransferError> {
        Ok(PathProofResponse::Unsupported)
    }

    /// Cumulative transport commands, when the adapter exposes request metrics.
    fn transport_requests(&self) -> Option<u64> {
        None
    }

    /// Cumulative transport commands by operation name, when exposed.
    fn transport_operations(&self) -> Option<Vec<(String, u64)>> {
        None
    }

    /// Whether this transport supports bounded plaintext payload batches.
    fn supports_payload_batch(&self) -> bool {
        false
    }

    /// Whether metadata and payload commands can share a transport exchange.
    fn supports_object_payload_batch(&self) -> bool {
        false
    }

    /// Fetch metadata and, optionally, at most `max_bytes` plaintext bytes.
    /// Callers reserve that byte budget before calling. Batched payloads
    /// arrive as sliced frames resolved through `sources`; every supplied
    /// payload still passes the ordinary receiver identity and namespace
    /// verification.
    async fn object_payload_batch(
        &self,
        _keys: &[ObjectKey],
        _max_bytes: usize,
        _sources: &dyn SliceSources,
    ) -> Result<Option<TransferObjectBatch>, TransferError> {
        Ok(None)
    }

    /// Whether the source expands discovery requests with descendants.
    fn supports_discovery(&self) -> bool {
        false
    }

    /// Fetch the records of `keys` and, within the source's bounds, of
    /// their descendants, plus payloads it chooses to volunteer decoded
    /// through `sources`. `None` keeps ordinary batched discovery. The
    /// receiver keeps only descendants reachable from `keys` and verifies
    /// every record and payload exactly as if it had requested them.
    async fn discover(
        &self,
        _keys: &[ObjectKey],
        _sources: &dyn SliceSources,
    ) -> Result<Option<TransferDiscoveryAnswer>, TransferError> {
        Ok(None)
    }

    /// Fetch a bounded batch in record order as sliced frames resolved
    /// through `sources`. `None` retains streaming fallback. The receiver
    /// still verifies each payload identity and namespace.
    async fn payload_batch(
        &self,
        _records: &[ObjectRecord],
        _max_bytes: usize,
        _sources: &dyn SliceSources,
    ) -> Result<Option<Vec<Option<TransferBatchPayload>>>, TransferError> {
        Ok(None)
    }

    /// Open the exact plaintext payload identified by this immutable record.
    /// Split sources may supply a record from an independent metadata session;
    /// the receiver still verifies the returned bytes against that record.
    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError>;

    /// Open a complete Bao proof stream. Keep this session alive until the
    /// reader is dropped. Unsupported peers fail without an unverified fallback.
    async fn open_proof(
        &self,
        _record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        Err(TransferError::SourceTransport(
            "source does not support Bao streams".into(),
        ))
    }

    /// Open independently authenticated plaintext from a source. The record's
    /// payload digest must come from a trusted identity or verified metadata.
    async fn open_verified(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        Ok(self.open_proof(record).await?.map(|proof| {
            crate::verified::stream::decode(proof, record.payload(), record.payload_size())
                as TransferPayloadReader
        }))
    }

    /// Return the physical chunk map for one source record when chunk
    /// negotiation is supported.
    async fn chunks(
        &self,
        _record: &ObjectRecord,
    ) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        Ok(None)
    }

    /// Return physical chunk maps in record order, preserving duplicates and
    /// unsupported (`None`) entries. All maps belong to this retained session.
    ///
    /// Remote adapters should override this to batch network requests. The
    /// default overlaps at most 32 lookups and drops outstanding futures on
    /// error or cancellation. Implementations may split large inputs into
    /// bounded transport batches without changing the result order.
    async fn chunks_batch(
        &self,
        records: &[ObjectRecord],
    ) -> Result<Vec<Option<Vec<ChunkMeta>>>, TransferError> {
        stream::iter(records.iter().cloned())
            .map(|record| async move { self.chunks(&record).await })
            .buffered(32)
            .try_collect()
            .await
    }

    /// Optional read-only compressed chunk source. Retention is owned by this
    /// session, independently of the read transport. Adapters without chunk
    /// access return `None` and use complete plaintext streaming.
    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        None
    }

    /// Tell the source which complete closure the receiver holds in place of
    /// each one it wants. The source pairs the two trees by path and slices
    /// every payload against its counterpart. `None` keeps the session on
    /// whole payloads.
    async fn offer_slice_bases(
        &self,
        _pairs: &[SliceBasePair],
    ) -> Result<Option<SliceBases>, TransferError> {
        Ok(None)
    }

    /// Receive one payload as a sliced frame, resolving copies through
    /// `sources` and writing the plaintext to `sink`. The receiver verifies
    /// the written bytes against the record before publication. After
    /// [`SlicedReceipt::MissingSource`] the sink holds an unusable prefix.
    async fn receive_sliced(
        &self,
        _record: &ObjectRecord,
        _sources: &dyn SliceSources,
        _sink: &mut (dyn tokio::io::AsyncWrite + Send + Unpin),
    ) -> Result<SlicedReceipt, TransferError> {
        Ok(SlicedReceipt::Unsupported)
    }
}

/// A closure the receiver holds beside the one it is fetching, typically the
/// current and next target of one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceBasePair {
    /// Complete verified closure the receiver holds.
    pub held: ObjectKey,
    /// Closure the receiver is fetching.
    pub wanted: ObjectKey,
}

/// What a source paired after the receiver offered its closures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SliceBases {
    /// Wanted payloads that gained a held counterpart at the same path.
    pub hints: u64,
}

/// Outcome of asking a session for one sliced payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlicedReceipt {
    /// The transport does not slice; use another payload path.
    Unsupported,
    /// The source has no payload for the record.
    Absent,
    /// The frame was decoded completely into the sink.
    Received(SliceStats),
    /// The frame named a blob the receiver lacks; the sink must be discarded.
    MissingSource(BlobId),
}

/// Data a transfer session may read. Selection controls retention, independently
/// of destination roots and whether the receiver copies recursively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferSelection {
    /// Retain an unrestricted snapshot, including for split payload sources
    /// that do not have logical records for the supplied payloads.
    Snapshot,
    /// Retain these objects and named roots and their complete forward graphs.
    /// Names resolve at the session's revision. Callers must restrict object,
    /// payload, proof, and chunk reads to these graphs. Metadata lookups alone
    /// do not expand protection. Backends may conservatively retain more data.
    Selected {
        /// Exact immutable graph roots.
        objects: Vec<ObjectKey>,
        /// Names to resolve in the same snapshot used for transfer.
        roots: Vec<RootName>,
    },
}

impl TransferSelection {
    pub(crate) fn validate(&self) -> Result<(), TransferError> {
        if let Self::Selected { objects, roots } = self {
            if objects.len().saturating_add(roots.len()) > MAX_TRANSFER_REQUEST_OBJECTS {
                return Err(TransferError::SourceTransport(
                    "transfer selection has too many entries".into(),
                ));
            }
            let bytes = objects
                .iter()
                .map(|key| key.encode().len() + 8)
                .chain(roots.iter().map(|name| name.as_str().len() + 8))
                .try_fold(24usize, |total, size| total.checked_add(size));
            if bytes.is_none_or(|size| size > MAX_TRANSFER_REQUEST_BYTES) {
                return Err(TransferError::SourceTransport(
                    "transfer selection exceeds its byte limit".into(),
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn apply_to<PS: BlobStore, SS: MetadataStore>(
        &self,
        hold: &mut crate::RetentionHold<'_, PS, SS>,
    ) -> Result<(), TransferError> {
        self.validate()?;
        if let Self::Selected { objects, roots } = self {
            let mut selected: BTreeSet<_> = objects.iter().cloned().collect();
            for name in roots {
                let key = hold
                    .snapshot()
                    .root(name)
                    .await
                    .map_err(TransferError::SourceMetadata)?
                    .ok_or_else(|| TransferError::MissingSourceRoot(name.clone()))?;
                selected.insert(key);
            }
            hold.retain_only(selected).await?;
        }
        Ok(())
    }

    fn from_request(request: &TransferRequest) -> Self {
        Self::Selected {
            objects: request
                .objects
                .iter()
                .map(|object| object.key.clone())
                .collect(),
            roots: Vec::new(),
        }
    }
}

/// Something that can open a stable generic transfer source session.
#[async_trait]
pub trait TransferSource: Send + Sync {
    /// Acquire protection before exposing one stable revision. Named selections
    /// must resolve at that revision; destination policy is not part of selection.
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError>;
}

/// Read roots and records from one retained session and payloads from another.
///
/// The metadata session defines the source revision. The payload session may
/// have a different revision but must contain the exact immutable payloads
/// required by those records. Both sessions retain their repositories until
/// this session is dropped. Missing payloads do not fall back to metadata.
/// Receiver identity and namespace verification remain unchanged.
pub struct SplitTransferSession<'a> {
    metadata: Box<dyn TransferReadSession + 'a>,
    payloads: Box<dyn TransferReadSession + 'a>,
}

impl<'a> SplitTransferSession<'a> {
    /// Combine already-retained metadata and payload source sessions.
    pub fn new(
        metadata: Box<dyn TransferReadSession + 'a>,
        payloads: Box<dyn TransferReadSession + 'a>,
    ) -> Self {
        Self { metadata, payloads }
    }
}

#[async_trait]
impl TransferReadSession for SplitTransferSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.metadata.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.metadata.object(key).await
    }

    async fn objects(
        &self,
        keys: &[ObjectKey],
    ) -> Result<Vec<Option<ObjectRecord>>, TransferError> {
        self.metadata.objects(keys).await
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.metadata.root(name).await
    }

    // Use ordinary path discovery: remote proofs and combined object/payload
    // batches would read directory payloads from the metadata endpoint.

    fn transport_requests(&self) -> Option<u64> {
        match (
            self.metadata.transport_requests(),
            self.payloads.transport_requests(),
        ) {
            (Some(metadata), Some(payloads)) => Some(metadata.saturating_add(payloads)),
            _ => None,
        }
    }

    fn supports_payload_batch(&self) -> bool {
        self.payloads.supports_payload_batch()
    }

    async fn payload_batch(
        &self,
        records: &[ObjectRecord],
        max_bytes: usize,
        sources: &dyn SliceSources,
    ) -> Result<Option<Vec<Option<TransferBatchPayload>>>, TransferError> {
        self.payloads
            .payload_batch(records, max_bytes, sources)
            .await
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.payloads.open_payload(record).await
    }

    async fn open_proof(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.payloads.open_proof(record).await
    }

    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.payloads.chunks(record).await
    }

    async fn chunks_batch(
        &self,
        records: &[ObjectRecord],
    ) -> Result<Vec<Option<Vec<ChunkMeta>>>, TransferError> {
        self.payloads.chunks_batch(records).await
    }

    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        self.payloads.as_chunk_source()
    }

    async fn offer_slice_bases(
        &self,
        pairs: &[SliceBasePair],
    ) -> Result<Option<SliceBases>, TransferError> {
        self.payloads.offer_slice_bases(pairs).await
    }

    async fn receive_sliced(
        &self,
        record: &ObjectRecord,
        sources: &dyn SliceSources,
        sink: &mut (dyn tokio::io::AsyncWrite + Send + Unpin),
    ) -> Result<SlicedReceipt, TransferError> {
        self.payloads.receive_sliced(record, sources, sink).await
    }
}

struct RepositoryTransferSession<'a, PS, SS> {
    repository: &'a Repository<PS, SS>,
    hold: crate::RetentionHold<'a, PS, SS>,
}

#[async_trait]
impl<PS, SS> TransferReadSession for RepositoryTransferSession<'_, PS, SS>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    fn revision(&self) -> RepositoryRevision {
        self.hold.snapshot().revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.hold
            .snapshot()
            .object(key)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn objects(
        &self,
        keys: &[ObjectKey],
    ) -> Result<Vec<Option<ObjectRecord>>, TransferError> {
        self.hold
            .snapshot()
            .object_batch(keys)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.hold
            .snapshot()
            .root(name)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.repository
            .payloads()
            .open_read(&record.payload())
            .await
            .map(|reader| reader.map(|reader| reader as TransferPayloadReader))
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    async fn open_proof(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.repository
            .payloads()
            .open_proof(&record.payload(), record.payload_size())
            .await
            .map(|reader| reader.map(|reader| reader as TransferPayloadReader))
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.repository
            .payloads()
            .chunks(&record.payload())
            .await
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        self.repository.payloads().as_chunk_source()
    }
}

impl<PS: BlobStore, SS: MetadataStore> Repository<PS, SS> {
    pub(crate) async fn transfer_hold(
        &self,
        selection: &TransferSelection,
    ) -> Result<crate::RetentionHold<'_, PS, SS>, TransferError> {
        selection.validate()?;
        if let TransferSelection::Selected { objects, roots } = selection
            && roots.is_empty()
        {
            return Ok(self
                .retention_hold_for(&objects.iter().cloned().collect())
                .await?);
        }
        let mut hold = self.retention_hold().await?;
        selection.apply_to(&mut hold).await?;
        Ok(hold)
    }
}

#[async_trait]
impl<PS, SS> TransferSource for Repository<PS, SS>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError> {
        let hold = self.transfer_hold(&selection).await?;
        Ok(Box::new(RepositoryTransferSession {
            repository: self,
            hold,
        }))
    }
}

/// Receiver policy for one transfer. It never changes the frozen request
/// encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransferOptions {
    /// How discovery treats matching, verified destination closures.
    pub discovery: TransferDiscovery,
}

impl TransferOptions {
    /// Select how discovery treats matching, verified destination closures.
    pub fn with_discovery(mut self, discovery: TransferDiscovery) -> Self {
        self.discovery = discovery;
        self
    }
}

/// An already-open session used as a transfer source.
///
/// Opening it lends out the held session: its revision and retention were
/// bound when it was opened, so its selection must already cover what the
/// transfer requests. Use it when a caller resolves names through
/// [`TransferReadSession::root`] before constructing the exact request, so the
/// same stable revision serves both.
#[derive(Clone, Copy)]
pub struct HeldSession<'a>(pub &'a dyn TransferReadSession);

#[async_trait]
impl TransferSource for HeldSession<'_> {
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError> {
        selection.validate()?;
        Ok(Box::new(self.0))
    }
}

/// Transfer selected objects or closures into a repository.
///
/// Successful bounded batches are intentionally durable even if a later
/// source boundary fails. No requested destination root changes until every
/// discovery and receiver verification step succeeds.
#[tracing::instrument(
    name = "transfer",
    skip_all,
    fields(objects = request.objects.len(), roots = request.roots.len(), source_revision = tracing::field::Empty)
)]
pub async fn transfer<PS, SS>(
    source: &dyn TransferSource,
    destination: &Repository<PS, SS>,
    request: TransferRequest,
    options: TransferOptions,
) -> Result<TransferResult, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    validate_request(destination, &request)?;
    let source = source
        .begin_transfer(TransferSelection::from_request(&request))
        .await?;
    tracing::Span::current().record(
        "source_revision",
        tracing::field::display(source.revision()),
    );
    transfer_session_inner(
        source.as_ref(),
        destination,
        request,
        None,
        options.discovery,
    )
    .await
}

/// Resolve one path beneath a named filesystem root and transfer only the
/// selected file or directory closure.
///
/// Directory ancestors are verified against one stable source snapshot and
/// discarded; the spine is verified under every discovery policy. If
/// `destination_root` is supplied, the selected closure is installed under
/// that explicit destination-owned name only after its closure and its
/// parent-advertised size verify. Symlinks are returned as inline metadata and
/// cannot be rooted because they have no object key.
#[tracing::instrument(
    name = "transfer.path",
    skip_all,
    fields(source_revision = tracing::field::Empty, components = tracing::field::Empty)
)]
pub async fn transfer_path<PS, SS>(
    source: &dyn TransferSource,
    destination: &Repository<PS, SS>,
    source_root: &RootName,
    path: &str,
    destination_root: Option<RootName>,
    options: TransferOptions,
) -> Result<PathTransferResult, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let components = transfer_path_components(path)?;
    tracing::Span::current().record("components", components.len());
    let source = source
        .begin_transfer(TransferSelection::Selected {
            objects: Vec::new(),
            roots: vec![source_root.clone()],
        })
        .await?;
    let source = source.as_ref();
    tracing::Span::current().record(
        "source_revision",
        tracing::field::display(source.revision()),
    );
    let node = match source.path_proof(source_root, &components).await? {
        PathProofResponse::Unsupported => {
            let root = source
                .root(source_root)
                .await?
                .ok_or_else(|| TransferError::MissingSourceRoot(source_root.clone()))?;
            validate_path_root(&root)?;
            resolve_verified_path(source, destination, root, &components).await?
        }
        PathProofResponse::MissingRoot => {
            return Err(TransferError::MissingSourceRoot(source_root.clone()));
        }
        PathProofResponse::Proof(proof) => {
            resolve_verified_path_proof(source, destination, source_root, &components, proof)
                .await?
        }
    };
    let Some(node) = node else {
        return Ok(PathTransferResult {
            node: None,
            transfer: None,
        });
    };
    let target = match &node {
        Node::Directory { digest, .. } => Some(ObjectKey::directory(*digest)),
        Node::File { digest, .. } => Some(ObjectKey::blob(*digest)),
        Node::Symlink { .. } => None,
    };
    let Some(target) = target else {
        if destination_root.is_some() {
            return Err(TransferError::InvalidRequest(
                "an inline symlink cannot be installed as a destination root".to_owned(),
            ));
        }
        return Ok(PathTransferResult {
            node: Some(node),
            transfer: None,
        });
    };

    let roots = destination_root
        .map(|name| DestinationRoot {
            name,
            target: target.clone(),
        })
        .into_iter()
        .collect();
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: target,
            recursive: true,
        }],
        roots,
    };
    validate_request(destination, &request)?;
    let transfer =
        transfer_session_inner(source, destination, request, Some(&node), options.discovery)
            .await?;
    Ok(PathTransferResult {
        node: Some(node),
        transfer: Some(transfer),
    })
}

fn validate_path_root(root: &ObjectKey) -> Result<(), TransferError> {
    if root.namespace().as_str() != crate::DIRECTORY_NAMESPACE {
        return Err(TransferError::InvalidRequest(format!(
            "path selection requires a `{}` source root, got `{}`",
            crate::DIRECTORY_NAMESPACE,
            root.namespace()
        )));
    }
    Ok(())
}

fn transfer_path_components(path: &str) -> Result<Vec<PathComponent>, TransferError> {
    if path.len() > MAX_TRANSFER_REQUEST_BYTES {
        return Err(TransferError::InvalidRequest(format!(
            "path exceeds {MAX_TRANSFER_REQUEST_BYTES} bytes"
        )));
    }
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            PathComponent::try_from(component).map_err(|error| {
                TransferError::InvalidRequest(format!(
                    "invalid path component `{component}`: {error}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() > MAX_TRANSFER_REQUEST_OBJECTS {
        return Err(TransferError::InvalidRequest(format!(
            "path has {} components, limit is {MAX_TRANSFER_REQUEST_OBJECTS}",
            components.len()
        )));
    }
    Ok(components)
}

async fn resolve_verified_path<PS, SS>(
    source: &dyn TransferReadSession,
    destination: &Repository<PS, SS>,
    root: ObjectKey,
    components: &[PathComponent],
) -> Result<Option<Node>, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let mut key = root;
    let mut expected_size = None;
    for index in 0..=components.len() {
        let directory = read_verified_source_directory(source, destination, &key).await?;
        if let Some(expected) = expected_size
            && directory.size() != expected
        {
            return Err(TransferError::InvalidSourceSelection {
                key,
                message: format!(
                    "parent advertises {expected} descendants, child contains {}",
                    directory.size()
                ),
            });
        }
        if index == components.len() {
            return Ok(Some(Node::Directory {
                digest: crate::DirectoryId::new(
                    key.native_digest()
                        .expect("directory keys have a verified 32-byte native identifier"),
                ),
                size: directory.size(),
            }));
        }
        let Some(node) = directory.get(&components[index]).cloned() else {
            return Ok(None);
        };
        if index + 1 == components.len() {
            return Ok(Some(node));
        }
        match node {
            Node::Directory { digest, size } => {
                key = ObjectKey::directory(digest);
                expected_size = Some(size);
            }
            Node::File { .. } | Node::Symlink { .. } => return Ok(None),
        }
    }
    unreachable!("path resolution returns from its bounded loop")
}

async fn resolve_verified_path_proof<PS, SS>(
    source: &dyn TransferReadSession,
    destination: &Repository<PS, SS>,
    source_root: &RootName,
    components: &[PathComponent],
    proof: PathProof,
) -> Result<Option<Node>, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    if proof.revision != source.revision() {
        return Err(TransferError::SourceTransport(format!(
            "path proof revision {} does not match retained source revision {}",
            proof.revision,
            source.revision()
        )));
    }
    if &proof.root_name != source_root {
        return Err(TransferError::SourceTransport(format!(
            "path proof root name `{}` does not match requested root `{source_root}`",
            proof.root_name
        )));
    }
    validate_path_root(&proof.root)?;
    if proof.directories.is_empty() {
        return Err(TransferError::InvalidSourceSelection {
            key: proof.root,
            message: "path proof contains no root directory".to_owned(),
        });
    }

    let mut key = proof.root;
    let mut expected_size = None;
    let mut directories = proof.directories.into_iter();
    for index in 0..=components.len() {
        let entry = directories
            .next()
            .ok_or_else(|| TransferError::InvalidSourceSelection {
                key: key.clone(),
                message: "path proof ended before resolution completed".to_owned(),
            })?;
        let directory =
            verify_source_directory(destination, &key, entry.record, entry.payload).await?;
        if let Some(expected) = expected_size
            && directory.size() != expected
        {
            return Err(TransferError::InvalidSourceSelection {
                key,
                message: format!(
                    "parent advertises {expected} descendants, child contains {}",
                    directory.size()
                ),
            });
        }

        let resolved = if index == components.len() {
            Some(Node::Directory {
                digest: crate::DirectoryId::new(
                    key.native_digest()
                        .expect("directory keys have a verified 32-byte native identifier"),
                ),
                size: directory.size(),
            })
        } else {
            let Some(node) = directory.get(&components[index]).cloned() else {
                require_proof_end(&key, &mut directories)?;
                return Ok(None);
            };
            if index + 1 == components.len() {
                Some(node)
            } else {
                match node {
                    Node::Directory { digest, size } => {
                        key = ObjectKey::directory(digest);
                        expected_size = Some(size);
                        None
                    }
                    Node::File { .. } | Node::Symlink { .. } => {
                        require_proof_end(&key, &mut directories)?;
                        return Ok(None);
                    }
                }
            }
        };
        if let Some(node) = resolved {
            require_proof_end(&key, &mut directories)?;
            return Ok(Some(node));
        }
    }
    unreachable!("path proof resolution returns from its bounded loop")
}

fn require_proof_end(
    key: &ObjectKey,
    directories: &mut impl Iterator<Item = PathProofEntry>,
) -> Result<(), TransferError> {
    if directories.next().is_some() {
        Err(TransferError::InvalidSourceSelection {
            key: key.clone(),
            message: "path proof contains trailing directory records".to_owned(),
        })
    } else {
        Ok(())
    }
}

async fn read_verified_source_directory<PS, SS>(
    source: &dyn TransferReadSession,
    destination: &Repository<PS, SS>,
    key: &ObjectKey,
) -> Result<Directory, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let record = source
        .object(key)
        .await?
        .ok_or_else(|| TransferError::IncompleteSource { key: key.clone() })?;
    if record.key() != key {
        return Err(TransferError::SourceTransport(format!(
            "source returned record {} for requested {key}",
            record.key()
        )));
    }
    let limit = destination
        .limits()
        .max_metadata_bytes
        .min(destination.limits().max_payload_bytes);
    if record.payload_size() > limit {
        return Err(TransferError::Destination(RepositoryError::LimitExceeded(
            format!(
                "source directory {} is {} bytes, limit is {limit}",
                record.key(),
                record.payload_size()
            ),
        )));
    }
    let reader = source
        .open_payload(&record)
        .await?
        .ok_or_else(|| TransferError::IncompleteSource { key: key.clone() })?;
    let read_limit = record.payload_size().saturating_add(1);
    let mut encoded = Vec::new();
    reader
        .take(read_limit)
        .read_to_end(&mut encoded)
        .await
        .map_err(|error| TransferError::SourcePayload {
            key: key.clone(),
            error: error.into(),
        })?;
    if encoded.len() as u64 != record.payload_size() {
        return Err(TransferError::SourceTransport(format!(
            "source payload for {key} has {} bytes, record declares {}",
            encoded.len(),
            record.payload_size()
        )));
    }
    verify_source_directory(destination, key, record, encoded).await
}

async fn verify_source_directory<PS, SS>(
    destination: &Repository<PS, SS>,
    key: &ObjectKey,
    record: ObjectRecord,
    encoded: Vec<u8>,
) -> Result<Directory, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    if record.key() != key {
        return Err(TransferError::SourceTransport(format!(
            "source returned record {} for requested {key}",
            record.key()
        )));
    }
    let limit = destination
        .limits()
        .max_metadata_bytes
        .min(destination.limits().max_payload_bytes);
    if record.payload_size() > limit {
        return Err(TransferError::Destination(RepositoryError::LimitExceeded(
            format!(
                "source directory {} is {} bytes, limit is {limit}",
                record.key(),
                record.payload_size()
            ),
        )));
    }
    if encoded.len() as u64 != record.payload_size() {
        return Err(TransferError::SourceTransport(format!(
            "source payload for {key} has {} bytes, record declares {}",
            encoded.len(),
            record.payload_size()
        )));
    }
    let mut cursor = std::io::Cursor::new(encoded.as_slice());
    let verified = destination
        .formats()
        .verify(key, &mut cursor, destination.limits())
        .await
        .map_err(|error| TransferError::SourceFormat {
            key: key.clone(),
            error,
        })?;
    if verified.record() != &record {
        return Err(TransferError::SourceTransport(format!(
            "source payload for {key} reproduces a different immutable record"
        )));
    }
    Directory::decode(&encoded).map_err(|error| {
        TransferError::SourceTransport(format!("source directory {key} cannot be decoded: {error}"))
    })
}

#[tracing::instrument(
    name = "transfer.execute",
    skip_all,
    fields(
        source_revision = %source.revision(),
        objects = request.objects.len(),
        roots = request.roots.len(),
        path_selected = selected_node.is_some()
    )
)]
async fn transfer_session_inner<PS, SS>(
    source: &dyn TransferReadSession,
    destination: &Repository<PS, SS>,
    request: TransferRequest,
    selected_node: Option<&Node>,
    discovery: TransferDiscovery,
) -> Result<TransferResult, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let mutation = destination.mutation_session().await?;
    transfer_into_mutation(source, &mutation, request, selected_node, discovery).await
}

impl<PS: BlobStore, SS: MetadataStore> MutationSession<'_, PS, SS> {
    /// Transfer verified content within this mutation lifetime. Reused selections
    /// and published checkpoints remain retained until the session is dropped,
    /// including requests without destination roots.
    #[cfg(feature = "experimental")]
    pub async fn transfer_from(
        &self,
        source: &dyn TransferSource,
        request: TransferRequest,
    ) -> Result<TransferResult, TransferError> {
        validate_request(self.repository(), &request)?;
        let source = source
            .begin_transfer(TransferSelection::from_request(&request))
            .await?;
        transfer_into_mutation(
            source.as_ref(),
            self,
            request,
            None,
            TransferDiscovery::Exhaustive,
        )
        .await
    }
}

async fn transfer_into_mutation<PS: BlobStore, SS: MetadataStore>(
    source: &dyn TransferReadSession,
    mutation: &MutationSession<'_, PS, SS>,
    request: TransferRequest,
    selected_node: Option<&Node>,
    discovery: TransferDiscovery,
) -> Result<TransferResult, TransferError> {
    let mut receiver = Receiver::open(source, mutation, &request, discovery).await?;
    while let Some(pending) = receiver.next_batch().await? {
        let round = receiver.describe(pending).await?;
        receiver.receive(round).await?;
    }
    receiver.finish(&request, selected_node).await
}

/// Receiver-side state of one transfer into a mutation session.
///
/// A transfer runs in rounds. [`next_batch`](Self::next_batch) takes the next
/// bounded batch of keys from the traversal, [`describe`](Self::describe)
/// pairs each with its source and destination records and queues the links of
/// recursive selections, and [`receive`](Self::receive) fetches, verifies and
/// stages the payloads the destination lacks, publishing checkpoints as
/// staging batches fill. [`finish`](Self::finish) publishes the remainder and
/// installs the requested roots.
struct Receiver<'a, 'r, PS: BlobStore, SS: MetadataStore> {
    source: &'a dyn TransferReadSession,
    mutation: &'a MutationSession<'r, PS, SS>,
    discovery: TransferDiscovery,
    budget: crate::byte_budget::ByteBudget,
    /// Bytes one payload batch may carry, leaving a read buffer free.
    batch_bytes: usize,
    concurrency: Semaphore,
    slice_sources: sliced::BlobSliceSources<&'a PS>,
    /// Destination state read by discovery. Released before every
    /// publication and reopened when discovery resumes.
    snapshot: Option<std::sync::Arc<dyn crate::metadata::MetadataSnapshot>>,
    area: crate::spill::SpillArea,
    queue: TraversalQueue,
    expanded: SpillSet<ObjectKey>,
    /// Only explicit selections can be shallow, so this set is bounded by the
    /// request's 4,096-item / 4 MiB limits. Recursive visits share one spill
    /// set rather than duplicating every graph key in a second database.
    shallow: BTreeSet<ObjectKey>,
    staged: Vec<crate::repository::StagedObject<'a>>,
    published_objects: u64,
    physical: PhysicalProgress,
    discovered: DiscoveredCache,
}

/// Records and payloads a discovering source volunteered ahead of the
/// batches that will ask for them. Payloads are bounded by one batch budget,
/// reserved once; records by the 4,096-object answer bound per answer and the
/// traversal limit overall, since every cached key is one the source claims
/// lies in the requested closure.
#[derive(Default)]
struct DiscoveredCache {
    records: BTreeMap<ObjectKey, ObjectRecord>,
    payloads: BTreeMap<ObjectKey, TransferBatchPayload>,
    bytes: usize,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// One discovery round: the keys it visited, the records the destination
/// needs, and any payloads fetched together with the records.
struct Round {
    keys: Vec<ObjectKey>,
    transferable: Vec<(ObjectKey, ObjectRecord)>,
    prefetched: BTreeMap<ObjectKey, Option<TransferBatchPayload>>,
    prefetch_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl<'a, 'r, PS: BlobStore, SS: MetadataStore> Receiver<'a, 'r, PS, SS> {
    async fn open(
        source: &'a dyn TransferReadSession,
        mutation: &'a MutationSession<'r, PS, SS>,
        request: &TransferRequest,
        discovery: TransferDiscovery,
    ) -> Result<Self, TransferError> {
        let destination = mutation.repository();
        // Matching records (including verified closures) bypass publication, so
        // acquire their retention before discovery reads destination state. Object
        // pins also cover descendants without adding every graph key to the pin.
        mutation
            .retain_objects(request.objects.iter().map(|selected| selected.key.clone()))
            .await?;
        let budget =
            crate::byte_budget::ByteBudget::new(destination.limits().max_transfer_in_flight_bytes);
        // Leave at least one rounded read-buffer reservation available while
        // the prefetched batch lives. Tiny budgets keep the streaming path.
        let unit = 64 * 1024;
        let buffer = destination.limits().read_buffer_bytes.max(1).div_ceil(unit) * unit;
        let batch_bytes = destination
            .limits()
            .max_transfer_in_flight_bytes
            .saturating_sub(buffer)
            .min(MAX_PAYLOAD_BATCH_BYTES)
            / unit
            * unit;
        // Bind the exact state-owned payload catalog before probing destination
        // payloads. In coordinated repositories the standalone pack pointer is
        // merely a recovery aid; consulting it once per missing payload would be
        // both stale and prohibitively expensive on request-priced object stores.
        let snapshot = destination.synchronized_snapshot().await?;

        // The roots this transfer will replace are complete verified closures the
        // destination holds, so the source may slice new payloads against them.
        let mut pairs = Vec::new();
        for root in &request.roots {
            let current = snapshot
                .root(&root.name)
                .await
                .map_err(RepositoryError::from)?;
            if let Some(held) = current
                && held != root.target
            {
                let pair = SliceBasePair {
                    held,
                    wanted: root.target.clone(),
                };
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
        if !pairs.is_empty()
            && let Some(paired) = source.offer_slice_bases(&pairs).await?
        {
            tracing::debug!(
                roots = pairs.len(),
                hints = paired.hints,
                "source paired destination closures as slice bases"
            );
        }

        let area = destination.spill_area();
        let mut queue = TraversalQueue::new(area.clone());
        for selected in &request.objects {
            // A queued edge is recursive; a lone key is shallow. An explicitly
            // recursive selection starts with a self-edge, preserving the exact
            // FIFO order even when a later selection upgrades a shallow key.
            let from = selected.recursive.then(|| selected.key.clone());
            queue
                .push((from, selected.key.clone()))
                .await
                .map_err(RepositoryError::from)?;
        }
        Ok(Self {
            source,
            mutation,
            discovery,
            budget,
            batch_bytes,
            concurrency: Semaphore::new(MAX_CONCURRENT_PHYSICAL_TRANSFERS),
            // Batched and streamed sliced payloads resolve copies from this store.
            slice_sources: sliced::BlobSliceSources::new(destination.payloads()),
            snapshot: Some(snapshot),
            expanded: SpillSet::new(area.clone(), "transfer-expanded"),
            area,
            queue,
            shallow: BTreeSet::new(),
            staged: Vec::new(),
            published_objects: 0,
            physical: PhysicalProgress::default(),
            discovered: DiscoveredCache::default(),
        })
    }

    /// Take the next bounded batch of unvisited keys, each marked recursive
    /// when a recursive edge reached it. `None` ends discovery.
    async fn next_batch(&mut self) -> Result<Option<Vec<(ObjectKey, bool)>>, TransferError> {
        let limits = self.mutation.repository().limits();
        let mut pending = Vec::<(ObjectKey, bool)>::new();
        let mut pending_index = BTreeMap::<ObjectKey, usize>::new();
        while pending.len() < MAX_TRANSFER_DISCOVERY_BATCH_OBJECTS {
            let Some((from, key)) = self.queue.pop().await.map_err(RepositoryError::from)? else {
                break;
            };
            let recursive = from.is_some();
            if recursive {
                if !self
                    .expanded
                    .insert(key.clone())
                    .await
                    .map_err(RepositoryError::from)?
                {
                    continue;
                }
                self.shallow.remove(&key);
            } else if self
                .expanded
                .contains(&key)
                .await
                .map_err(RepositoryError::from)?
                || !self.shallow.insert(key.clone())
            {
                continue;
            }
            if self.expanded.len().saturating_add(self.shallow.len()) > limits.max_traversal_objects
            {
                return Err(TransferError::InvalidRequest(format!(
                    "discovery exceeds {} objects",
                    limits.max_traversal_objects
                )));
            }

            if let Some(index) = pending_index.get(&key).copied() {
                pending[index].1 |= recursive;
            } else {
                pending_index.insert(key.clone(), pending.len());
                pending.push((key, recursive));
            }
        }
        Ok((!pending.is_empty()).then_some(pending))
    }

    /// Pair each key with its source and destination records, queue the links
    /// of recursive keys, and keep the records the destination must receive.
    async fn describe(&mut self, pending: Vec<(ObjectKey, bool)>) -> Result<Round, TransferError> {
        let destination = self.mutation.repository();
        let source = self.source;
        let keys: Vec<_> = pending.iter().map(|(key, _)| key.clone()).collect();
        tracing::debug!(
            batch_objects = keys.len(),
            discovered = self.expanded.len().saturating_add(self.shallow.len()),
            "fetching transfer discovery batch"
        );
        if self.snapshot.is_none() {
            self.snapshot = Some(
                destination
                    .metadata()
                    .snapshot()
                    .await
                    .map_err(RepositoryError::from)?,
            );
        }
        let snapshot = self.snapshot.clone().expect("opened destination snapshot");
        let destination_records = snapshot
            .object_batch(&keys)
            .await
            .map_err(RepositoryError::from)?;
        if destination_records.len() != pending.len() {
            return Err(TransferError::Destination(RepositoryError::Metadata(
                MetadataError::Corruption(format!(
                    "destination returned {} object answers for {} batched keys",
                    destination_records.len(),
                    pending.len()
                )),
            )));
        }

        let mut prefetched = BTreeMap::new();
        let mut prefetch_permit = None;
        // Keys the source already described through an earlier discovery
        // answer need no request; the rest go out in one discovery request
        // when the source expands them, or through the batch paths below.
        let missing: Vec<ObjectKey> = keys
            .iter()
            .filter(|key| !self.discovered.records.contains_key(*key))
            .cloned()
            .collect();
        let answer = if !missing.is_empty() && source.supports_discovery() {
            source.discover(&missing, &self.slice_sources).await?
        } else {
            None
        };
        if let Some(answer) = answer {
            self.accept_answer(&missing, answer).await?;
        }
        let cached_all = keys
            .iter()
            .all(|key| self.discovered.records.contains_key(key));
        let mut can_prefetch = !cached_all
            && source.supports_object_payload_batch()
            && self.batch_bytes >= MAX_PAYLOAD_BATCH_BYTES
            && keys.len() <= MAX_PAYLOAD_BATCH_OBJECTS
            && destination_records.iter().all(Option::is_none);
        if can_prefetch {
            // These namespaces use the plaintext digest as their native ID.
            // Check physical presence too: a new logical alias can already have
            // its payload locally. Other namespaces keep metadata-first lookup.
            for key in &keys {
                if !matches!(
                    key.namespace().as_str(),
                    crate::object::BLOB_NAMESPACE | crate::object::DIRECTORY_NAMESPACE
                ) {
                    can_prefetch = false;
                    break;
                }
                let Ok(digest) = crate::Digest::try_from(key.native_id()) else {
                    can_prefetch = false;
                    break;
                };
                if destination
                    .payloads()
                    .has(&crate::BlobId::new(digest))
                    .await
                    .map_err(RepositoryError::Payload)?
                {
                    can_prefetch = false;
                    break;
                }
            }
        }
        let combined = if can_prefetch {
            prefetch_permit = Some(self.budget.reserve(self.batch_bytes).await);
            source
                .object_payload_batch(&keys, self.batch_bytes, &self.slice_sources)
                .await?
        } else {
            None
        };
        // Cached records stay: one blob may be linked under several names, and
        // the traversal sets already bound how many keys are ever visited.
        let source_records = if cached_all {
            keys.iter()
                .map(|key| self.discovered.records.get(key).cloned())
                .collect::<Vec<_>>()
        } else if let Some(batch) = combined {
            if let Some(readers) = batch.payloads {
                if readers.len() != keys.len() {
                    return Err(TransferError::SourceTransport(
                        "combined payload answer count mismatch".into(),
                    ));
                }
                prefetched.extend(keys.iter().cloned().zip(readers));
            } else {
                prefetch_permit = None;
            }
            batch.records
        } else {
            prefetch_permit = None;
            let mut records = Vec::with_capacity(keys.len());
            let fetched = source.objects(&missing).await?;
            if fetched.len() != missing.len() {
                return Err(TransferError::SourceTransport(format!(
                    "source returned {} object answers for {} batched keys",
                    fetched.len(),
                    missing.len()
                )));
            }
            let mut fetched = missing.iter().zip(fetched).collect::<BTreeMap<_, _>>();
            for key in &keys {
                records.push(match self.discovered.records.get(key).cloned() {
                    Some(record) => Some(record),
                    None => fetched.remove(key).flatten(),
                });
            }
            records
        };
        if source_records.len() != pending.len() {
            return Err(TransferError::SourceTransport(format!(
                "source returned {} object answers for {} batched keys",
                source_records.len(),
                pending.len()
            )));
        }
        let mut complete = vec![false; keys.len()];
        if self.discovery == TransferDiscovery::ReuseVerified {
            // Leaves and shallow selections have no descendants to prune.
            // Avoid adding a closure query to flat or mostly-new transfers.
            let candidates: Vec<_> = (0..keys.len())
                .filter(|&index| {
                    pending[index].1
                        && destination_records[index].is_some()
                        && source_records[index]
                            .as_ref()
                            .is_some_and(|record| !record.links().is_empty())
                })
                .collect();
            if !candidates.is_empty() {
                let reuse_keys: Vec<_> = candidates.iter().map(|&i| keys[i].clone()).collect();
                let flags = snapshot
                    .validated_closures(&reuse_keys)
                    .await
                    .map_err(RepositoryError::from)?;
                if flags.len() != candidates.len() {
                    return Err(RepositoryError::Metadata(MetadataError::Corruption(
                        "destination returned an incorrect validated-closure batch length"
                            .to_owned(),
                    ))
                    .into());
                }
                for (index, flag) in candidates.into_iter().zip(flags) {
                    complete[index] = flag;
                }
            }
        }

        let mut transferable = Vec::with_capacity(pending.len());
        for ((((key, recursive), source_record), destination_record), complete) in pending
            .into_iter()
            .zip(source_records)
            .zip(destination_records)
            .zip(complete)
        {
            let source_record = source_record
                .ok_or_else(|| TransferError::IncompleteSource { key: key.clone() })?;
            if source_record.key() != &key {
                return Err(TransferError::SourceTransport(format!(
                    "source returned record {} for requested {key}",
                    source_record.key()
                )));
            }

            if complete && destination_record.as_ref() == Some(&source_record) {
                continue;
            }

            if recursive {
                for link in source_record.links() {
                    self.queue
                        .push((Some(key.clone()), link.clone()))
                        .await
                        .map_err(RepositoryError::from)?;
                }
            }

            match destination_record {
                Some(existing) if existing == source_record => continue,
                Some(_) => return Err(TransferError::ImmutableConflict(key)),
                None => {}
            }
            transferable.push((key, source_record));
        }
        Ok(Round {
            keys,
            transferable,
            prefetched,
            prefetch_permit,
        })
    }

    /// Cache what a discovery answer volunteered for the keys it was asked.
    async fn accept_answer(
        &mut self,
        missing: &[ObjectKey],
        answer: TransferDiscoveryAnswer,
    ) -> Result<(), TransferError> {
        let accepted = accept_discovery(missing, answer)?;
        let cache = &mut self.discovered;
        // A key is visited at most twice, once shallow and once recursive, so
        // entries cannot be dropped as they are read. Each round drops what it
        // finished with instead, which keeps the map to the answers still in
        // flight; the cap is only a backstop against an answer describing far
        // more than is ever asked for.
        for (key, record) in accepted.records {
            if cache.records.len() >= MAX_DISCOVERED_RECORDS {
                break;
            }
            cache.records.insert(key, record);
        }
        if cache.bytes == 0 && !accepted.payloads.is_empty() {
            cache.permit = Some(self.budget.reserve(DISCOVERED_PAYLOAD_BYTES).await);
        }
        for (key, payload) in accepted.payloads {
            let Some((reader, stats)) = payload else {
                continue;
            };
            let Some(size) = cache
                .records
                .get(&key)
                .map(|record| record.payload_size() as usize)
            else {
                continue;
            };
            if cache.bytes.saturating_add(size) > DISCOVERED_PAYLOAD_BYTES {
                // Over budget: fetched again later with its batch. Those
                // bytes cross the wire twice, so the bound below holds
                // several answers rather than one.
                continue;
            }
            cache.bytes += size;
            cache.payloads.insert(key, (reader, stats));
        }
        Ok(())
    }

    /// Fetch, verify and stage the payloads of one round, publishing a
    /// checkpoint each time a staging batch fills.
    async fn receive(&mut self, round: Round) -> Result<(), TransferError> {
        let Round {
            keys,
            transferable,
            mut prefetched,
            prefetch_permit,
        } = round;
        let destination = self.mutation.repository();
        let source = self.source;
        let batch_bytes = self.batch_bytes;
        let batch_size = destination.limits().max_batch_objects;
        if batch_size == 0 {
            return Err(TransferError::Destination(RepositoryError::LimitExceeded(
                "transfer requires a nonzero mutation batch limit".to_owned(),
            )));
        }
        let mut transferred = 0;
        while transferred < transferable.len() {
            // Always take at least one record, so a full staging batch that
            // has not been published yet cannot stall the round.
            let available = (batch_size - self.staged.len()).max(1);
            let limit = transferred
                .saturating_add(available)
                .min(transferable.len());
            let mut batch_permit = None;
            // A round carries what one payload batch can hold, plus records
            // no batch could hold at all: those stream, and streaming them
            // beside the batch costs no round trip of their own. A record
            // that merely did not fit this batch waits for the next round,
            // where it will, rather than becoming a request by itself.
            let batching =
                prefetch_permit.is_none() && source.supports_payload_batch() && batch_bytes > 0;
            let mut batchable = Vec::new();
            let mut batchable_bytes = 0usize;
            let mut end = transferred;
            while end < limit {
                let size =
                    usize::try_from(transferable[end].1.payload_size()).unwrap_or(usize::MAX);
                if batching && size <= batch_bytes {
                    if batchable.len() == MAX_PAYLOAD_BATCH_OBJECTS
                        || size > batch_bytes - batchable_bytes
                    {
                        break;
                    }
                    batchable_bytes += size;
                    batchable.push(end - transferred);
                }
                end += 1;
            }
            let end = end.max(transferred + 1).min(transferable.len());
            // Payloads a discovery answer volunteered come first, then the
            // batch fetched with this discovery round, then a payload batch
            // for what is still missing at the destination.
            let cache = &mut self.discovered;
            let mut readers: Vec<Option<TransferBatchPayload>> = transferable[transferred..end]
                .iter()
                .map(|(key, record)| {
                    let payload = cache.payloads.remove(key);
                    if payload.is_some() {
                        cache.bytes = cache.bytes.saturating_sub(record.payload_size() as usize);
                    }
                    payload
                })
                .collect();
            if prefetch_permit.is_some() {
                for (slot, (key, _)) in readers.iter_mut().zip(&transferable[transferred..end]) {
                    if slot.is_none() {
                        *slot = prefetched.remove(key).flatten();
                    }
                }
            } else if !batchable.is_empty() {
                // Logical records may be new while their payloads are already
                // present (e.g. aliases); do not request those bytes remotely.
                let mut missing = Vec::new();
                let mut positions = Vec::new();
                for &position in &batchable {
                    let record = &transferable[transferred + position].1;
                    if readers[position].is_none()
                        && !destination
                            .payloads()
                            .has(&record.payload())
                            .await
                            .map_err(RepositoryError::Payload)?
                    {
                        missing.push(record.clone());
                        positions.push(position);
                    }
                }
                if !missing.is_empty() {
                    batch_permit = Some(self.budget.reserve(batchable_bytes).await);
                    if let Some(payloads) = source
                        .payload_batch(&missing, batch_bytes, &self.slice_sources)
                        .await?
                    {
                        if payloads.len() != missing.len() {
                            return Err(TransferError::SourceTransport(
                                "payload batch answer count mismatch".into(),
                            ));
                        }
                        for (position, payload) in positions.into_iter().zip(payloads) {
                            readers[position] = payload;
                        }
                    } else {
                        batch_permit = None;
                    }
                }
            }
            let mutation = self.mutation;
            let budget = &self.budget;
            let concurrency = &self.concurrency;
            let slice_sources = &self.slice_sources;
            let mut pending_payloads = Vec::new();
            for ((key, source_record), prefetched) in
                transferable[transferred..end].iter().cloned().zip(readers)
            {
                pending_payloads.push(async move {
                    let progress = ensure_payload(
                        source,
                        mutation,
                        &source_record,
                        prefetched,
                        slice_sources,
                        budget,
                        concurrency,
                    )
                    .await?;
                    let verified = mutation
                        .stage_existing(key.clone(), source_record.payload())
                        .await?;
                    if verified.record() != &source_record {
                        return Err(TransferError::ImmutableConflict(key));
                    }
                    Ok(Some((verified, progress)))
                });
            }
            let results = stream::iter(pending_payloads)
                .buffered(MAX_CONCURRENT_PAYLOAD_TRANSFERS)
                .try_collect::<Vec<_>>()
                .await?;
            drop(batch_permit);
            transferred = end;
            for (verified, progress) in results.into_iter().flatten() {
                self.physical.merge(progress);
                self.staged.push(verified);
            }

            if self.staged.len() == batch_size {
                // Do not retain a destination read transaction across a write
                // that may checkpoint its WAL. Source retention is unchanged;
                // destination records are immutable under the mutation hold.
                drop(self.snapshot.take());
                let result = self.publish_staged().await?;
                tracing::debug!(
                    revision = %result.revision,
                    objects_inserted = result.objects_inserted,
                    "published transfer checkpoint"
                );
            }
        }
        drop(prefetch_permit);

        // This round is done with its keys, so any payload still cached for
        // one of them was never wanted: a record already present at the
        // destination, or one whose closure was reused. Releasing them keeps
        // the cache's budget available for the answers still to come, rather
        // than filling it once and then carrying no payloads at all.
        let cache = &mut self.discovered;
        for key in &keys {
            if cache.payloads.remove(key).is_some() {
                let size = cache
                    .records
                    .get(key)
                    .map_or(0, |record| record.payload_size() as usize);
                cache.bytes = cache.bytes.saturating_sub(size);
            }
            cache.records.remove(key);
        }
        Ok(())
    }

    /// Publish everything staged so far as one unrooted batch.
    async fn publish_staged(&mut self) -> Result<crate::metadata::CommitResult, TransferError> {
        let result = self
            .mutation
            .publish_unrooted(std::mem::take(&mut self.staged))
            .await?;
        self.published_objects = self
            .published_objects
            .checked_add(result.objects_inserted as u64)
            .ok_or_else(|| {
                TransferError::InvalidRequest("published object count overflow".to_owned())
            })?;
        Ok(result)
    }

    /// Publish the remainder, install the requested roots and report the
    /// outcome of every requested object.
    async fn finish(
        mut self,
        request: &TransferRequest,
        selected_node: Option<&Node>,
    ) -> Result<TransferResult, TransferError> {
        let destination = self.mutation.repository();
        // Discovery is finished. Release its read transaction before final
        // publication can checkpoint the destination WAL; the mutation session
        // continues to hold physical retention through publication and reporting.
        drop(self.snapshot.take());
        drop(std::mem::take(&mut self.discovered));
        if !self.staged.is_empty() {
            self.publish_staged().await?;
        }

        if let Some(node) = selected_node {
            verify_selected_node_size(destination, node).await?;
        }

        let root_changes = request
            .roots
            .iter()
            .map(|root| crate::RootChange::Set {
                name: root.name.clone(),
                target: root.target.clone(),
            })
            .collect();
        let destination_revision = if request.roots.is_empty() {
            destination
                .metadata()
                .snapshot()
                .await
                .map_err(RepositoryError::from)?
                .revision()
        } else {
            self.mutation
                .publish(Vec::new(), root_changes)
                .await?
                .revision
        };

        let mut requested = Vec::with_capacity(request.objects.len());
        for selected in &request.objects {
            // The mutation above verified whatever it named; this reports the
            // outcome rather than re-reading the whole graph a second time.
            requested.push(
                match destination
                    .verify_closure_incremental(&selected.key)
                    .await?
                {
                    ClosureStatus::Complete { .. } => {
                        RequestedStatus::Complete(selected.key.clone())
                    }
                    ClosureStatus::Missing { from: None, .. } => {
                        RequestedStatus::Missing(selected.key.clone())
                    }
                    ClosureStatus::Missing { .. } => {
                        RequestedStatus::Incomplete(selected.key.clone())
                    }
                    ClosureStatus::Invalid { .. } | ClosureStatus::Unsupported { .. } => {
                        RequestedStatus::Invalid(selected.key.clone())
                    }
                },
            );
        }

        let physical = &self.physical;
        tracing::info!(
            destination_revision = %destination_revision,
            discovered_objects = self.expanded.len().saturating_add(self.shallow.len()),
            published_objects = self.published_objects,
            payloads_sent = physical.payloads_sent,
            payloads_reused = physical.payloads_reused,
            chunks_sent = physical.chunks_sent,
            chunks_reused = physical.chunks_reused,
            slice_copy_bytes = physical.slice_copy_bytes,
            slice_literal_bytes = physical.slice_literal_bytes,
            spill_files = self.area.metrics().files_opened,
            spill_peak_bytes = self.area.metrics().peak_bytes,
            "transfer completed"
        );
        Ok(TransferResult {
            progress: TransferProgress {
                destination_revision,
                published_objects: self.published_objects,
                payloads_sent: physical.payloads_sent,
                payloads_reused: physical.payloads_reused,
                chunks_sent: physical.chunks_sent,
                chunks_reused: physical.chunks_reused,
                slice_copy_bytes: physical.slice_copy_bytes,
                slice_literal_bytes: physical.slice_literal_bytes,
                requested,
            },
        })
    }
}

async fn verify_selected_node_size<PS, SS>(
    destination: &Repository<PS, SS>,
    node: &Node,
) -> Result<(), TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    let (key, expected_size, directory) = match node {
        Node::File { digest, size, .. } => (ObjectKey::blob(*digest), *size, false),
        Node::Directory { digest, size } => (ObjectKey::directory(*digest), *size, true),
        Node::Symlink { .. } => return Ok(()),
    };
    let record = destination
        .metadata()
        .snapshot()
        .await
        .map_err(RepositoryError::from)?
        .object(&key)
        .await
        .map_err(RepositoryError::from)?
        .ok_or_else(|| TransferError::IncompleteSource { key: key.clone() })?;
    if !directory {
        if record.payload_size() != expected_size {
            return Err(TransferError::InvalidSourceSelection {
                key,
                message: format!(
                    "parent advertises {expected_size} bytes, selected file contains {}",
                    record.payload_size()
                ),
            });
        }
        return Ok(());
    }

    let reader = destination
        .payloads()
        .open_read(&record.payload())
        .await
        .map_err(RepositoryError::Payload)?
        .ok_or_else(|| {
            TransferError::Destination(RepositoryError::MissingPayload(record.payload()))
        })?;
    let limit = destination
        .limits()
        .max_metadata_bytes
        .min(destination.limits().max_payload_bytes);
    let mut encoded = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut encoded)
        .await
        .map_err(RepositoryError::Io)?;
    if encoded.len() as u64 > limit {
        return Err(TransferError::Destination(RepositoryError::LimitExceeded(
            format!("selected directory {key} exceeds {limit} bytes"),
        )));
    }
    let selected =
        Directory::decode(&encoded).map_err(|error| RepositoryError::Payload(error.into()))?;
    if selected.size() != expected_size {
        return Err(TransferError::InvalidSourceSelection {
            key,
            message: format!(
                "parent advertises {expected_size} descendants, selected directory contains {}",
                selected.size()
            ),
        });
    }
    Ok(())
}

/// Keep only what a discovery answer may legitimately contain: records with
/// distinct keys that are the requested keys or reachable from them through
/// links of accepted records, and payloads for those records. Everything
/// kept is still verified before it is staged, exactly like requested data.
fn accept_discovery(
    requested: &[ObjectKey],
    answer: TransferDiscoveryAnswer,
) -> Result<AcceptedDiscovery, TransferError> {
    if answer.records.len() > MAX_TRANSFER_REQUEST_OBJECTS {
        return Err(TransferError::SourceTransport(
            "discovery answer exceeds the record bound".into(),
        ));
    }
    let mut by_key = BTreeMap::new();
    for record in answer.records {
        if by_key.insert(record.key().clone(), record).is_some() {
            return Err(TransferError::SourceTransport(
                "discovery answer repeats a record".into(),
            ));
        }
    }
    let mut accepted = Vec::with_capacity(by_key.len());
    let mut queue: std::collections::VecDeque<ObjectKey> = requested.iter().cloned().collect();
    while let Some(key) = queue.pop_front() {
        let Some(record) = by_key.remove(&key) else {
            continue;
        };
        queue.extend(record.links().iter().cloned());
        accepted.push((key, record));
    }
    if !by_key.is_empty() {
        tracing::debug!(
            unrelated = by_key.len(),
            "ignored discovery records not reachable from the requested keys"
        );
    }
    let keys: BTreeSet<&ObjectKey> = accepted.iter().map(|(key, _)| key).collect();
    let payloads = answer
        .payloads
        .into_iter()
        .filter(|(key, _)| keys.contains(key))
        .collect();
    Ok(AcceptedDiscovery {
        records: accepted,
        payloads,
    })
}

/// The part of a discovery answer the receiver keeps.
struct AcceptedDiscovery {
    records: Vec<(ObjectKey, ObjectRecord)>,
    payloads: Vec<(ObjectKey, Option<TransferBatchPayload>)>,
}

fn validate_request<PS, SS>(
    destination: &Repository<PS, SS>,
    request: &TransferRequest,
) -> Result<(), TransferError> {
    if request.objects.len() > MAX_TRANSFER_REQUEST_OBJECTS {
        return Err(TransferError::InvalidRequest(format!(
            "{} selected objects exceeds limit {MAX_TRANSFER_REQUEST_OBJECTS}",
            request.objects.len()
        )));
    }
    if request.roots.len() > destination.limits().max_root_changes {
        return Err(TransferError::InvalidRequest(format!(
            "{} roots exceeds limit {}",
            request.roots.len(),
            destination.limits().max_root_changes
        )));
    }
    let mut encoded_size = 16usize;
    for selected in &request.objects {
        encoded_size = encoded_size
            .checked_add(selected.key.encode().len() + 1)
            .ok_or_else(|| TransferError::InvalidRequest("request size overflow".to_owned()))?;
    }
    for root in &request.roots {
        encoded_size = encoded_size
            .checked_add(root.name.as_str().len() + root.target.encode().len() + 16)
            .ok_or_else(|| TransferError::InvalidRequest("request size overflow".to_owned()))?;
    }
    if encoded_size > MAX_TRANSFER_REQUEST_BYTES {
        return Err(TransferError::InvalidRequest(format!(
            "request encodes to {encoded_size} bytes, limit is {MAX_TRANSFER_REQUEST_BYTES}"
        )));
    }
    Ok(())
}

#[derive(Default)]
struct PhysicalProgress {
    payloads_sent: u64,
    payloads_reused: u64,
    chunks_sent: u64,
    chunks_reused: u64,
    slice_copy_bytes: u64,
    slice_literal_bytes: u64,
}

impl PhysicalProgress {
    fn merge(&mut self, other: Self) {
        self.payloads_sent += other.payloads_sent;
        self.payloads_reused += other.payloads_reused;
        self.chunks_sent += other.chunks_sent;
        self.chunks_reused += other.chunks_reused;
        self.slice_copy_bytes += other.slice_copy_bytes;
        self.slice_literal_bytes += other.slice_literal_bytes;
    }
}

/// Reserved receiver memory while decoding one sliced frame: a literal
/// segment plus one copy piece, both bounded by the frame format.
const SLICE_RECEIVE_RESERVE_BYTES: usize = 2 * 1024 * 1024;

/// Records held from discovery answers before the batches that ask for them.
/// Rounds drop what they finished with, so this only bounds an answer that
/// describes far more of a closure than the receiver goes on to request.
const MAX_DISCOVERED_RECORDS: usize = 4 * MAX_TRANSFER_REQUEST_OBJECTS;

/// Payloads a single discovery answer may volunteer. The receiver has not
/// asked for these, and an answer that guesses wrong sends bytes the receiver
/// already holds, so this is deliberately narrower than a batch: measured on
/// a rebuilt closure, widening it to the batch bound sent 36 MB of payloads
/// the receiver threw away.
#[cfg(feature = "ssh")]
pub(crate) const DISCOVERY_ANSWER_PAYLOAD_OBJECTS: usize = 64;

/// Plaintext a single discovery answer may carry. The answer is already on
/// the critical path, so payloads riding in it cost no round trip; the bound
/// is the receiver memory holding them, not the batch convention.
pub(crate) const DISCOVERY_ANSWER_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Decoded plaintext held from discovery answers before the batches that will
/// stage it. An answer arrives while the previous one is still being staged,
/// so holding only one answer's worth drops payloads that were already sent
/// and pays for them again in a later batch.
const DISCOVERED_PAYLOAD_BYTES: usize = 4 * DISCOVERY_ANSWER_PAYLOAD_BYTES;

// Holding one answer's worth is not enough, and nothing else fails when it
// shrinks: on a rebuilt 349-path closure it dropped 405 payloads, 9.1 MB, that
// had already crossed the wire, and fetched them again in a later batch.
const _: () = assert!(
    DISCOVERED_PAYLOAD_BYTES >= 2 * DISCOVERY_ANSWER_PAYLOAD_BYTES,
    "a receiver must hold more than one discovery answer of volunteered payloads"
);

#[tracing::instrument(
    name = "transfer.payload",
    level = "debug",
    skip_all,
    fields(payload_bytes = record.payload_size())
)]
async fn ensure_payload<PS, SS>(
    source: &dyn TransferReadSession,
    destination: &MutationSession<'_, PS, SS>,
    record: &ObjectRecord,
    prefetched: Option<TransferBatchPayload>,
    slice_sources: &dyn SliceSources,
    transfer_budget: &crate::byte_budget::ByteBudget,
    transfer_concurrency: &Semaphore,
) -> Result<PhysicalProgress, TransferError>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    destination
        .write_scope()
        .run(async {
            let mut progress = PhysicalProgress::default();
            let payload = record.payload();
            if destination
                .repository()
                .payloads()
                .has(&payload)
                .await
                .map_err(RepositoryError::Payload)?
            {
                progress.payloads_reused += 1;
                tracing::debug!("reused complete destination payload");
                return Ok(progress);
            }

            if let (Some(source_sync), Some(destination_sync)) = (
                source.as_chunk_source(),
                destination.repository().payloads().as_blob_sync(),
            ) {
                let chunks = source.chunks(record).await?.ok_or_else(|| {
                    TransferError::IncompleteSource {
                        key: record.key().clone(),
                    }
                })?;
                if !chunks.is_empty() {
                    let missing = destination_sync
                        .missing_chunks(&chunks)
                        .await
                        .map_err(RepositoryError::Payload)?;
                    progress.chunks_reused += (chunks.len() - missing.len()) as u64;
                    let key = record.key().clone();
                    let copied = stream::iter(missing)
                        .map(|meta| {
                            let key = key.clone();
                            async move {
                                let _physical = transfer_concurrency
                                    .acquire()
                                    .await
                                    .expect("transfer concurrency semaphore remains open");
                                let _memory = transfer_budget
                                    .reserve(usize::try_from(meta.size).unwrap_or(usize::MAX))
                                    .await;
                                let bytes = source_sync
                                    .get_chunk(&meta.digest)
                                    .await
                                    .map_err(|error| TransferError::SourcePayload {
                                        key: key.clone(),
                                        error,
                                    })?
                                    .ok_or_else(|| TransferError::IncompleteSource { key })?;
                                destination_sync
                                    .put_chunk(&meta, bytes)
                                    .await
                                    .map_err(RepositoryError::Payload)?;
                                Ok::<(), TransferError>(())
                            }
                        })
                        .buffer_unordered(MAX_CONCURRENT_PHYSICAL_TRANSFERS)
                        .try_collect::<Vec<_>>()
                        .await?;
                    progress.chunks_sent += copied.len() as u64;
                    let chunk_count = chunks.len();
                    destination_sync
                        .put_manifest(&payload, chunks)
                        .await
                        .map_err(RepositoryError::Payload)?;
                    progress.payloads_sent += 1;
                    tracing::debug!(
                        chunks = chunk_count,
                        chunks_sent = progress.chunks_sent,
                        chunks_reused = progress.chunks_reused,
                        "transferred payload through chunk negotiation"
                    );
                    return Ok(progress);
                }
            }

            let _physical = transfer_concurrency
                .acquire()
                .await
                .expect("transfer concurrency semaphore remains open");
            if prefetched.is_none() {
                // Sliced frames resolve copies from blobs this destination
                // already holds; any other outcome keeps the plaintext path.
                let _memory = transfer_budget.reserve(SLICE_RECEIVE_RESERVE_BYTES).await;
                let mut writer = destination.repository().payloads().open_write().await;
                match source
                    .receive_sliced(record, slice_sources, &mut *writer)
                    .await?
                {
                    SlicedReceipt::Received(stats) => {
                        let (actual, _) = writer.close().await.map_err(RepositoryError::Payload)?;
                        if actual != payload {
                            return Err(TransferError::Destination(
                                RepositoryError::PayloadIdentityMismatch {
                                    expected: payload,
                                    actual,
                                },
                            ));
                        }
                        progress.payloads_sent += 1;
                        progress.slice_copy_bytes += stats.copy_bytes;
                        progress.slice_literal_bytes += stats.literal_bytes;
                        tracing::debug!(
                            copies = stats.copies,
                            copy_bytes = stats.copy_bytes,
                            literal_bytes = stats.literal_bytes,
                            frame_bytes = stats.frame_bytes,
                            "transferred and verified sliced payload"
                        );
                        return Ok(progress);
                    }
                    SlicedReceipt::MissingSource(missing) => {
                        tracing::debug!(
                            %missing,
                            "sliced payload named an absent source; streaming the whole payload"
                        );
                    }
                    SlicedReceipt::Unsupported | SlicedReceipt::Absent => {}
                }
                drop(writer);
            }
            let (mut reader, batched) = match prefetched {
                Some((reader, stats)) => (reader, Some(stats)),
                None => (
                    source.open_payload(record).await?.ok_or_else(|| {
                        TransferError::IncompleteSource {
                            key: record.key().clone(),
                        }
                    })?,
                    None,
                ),
            };
            let mut writer = destination.repository().payloads().open_write().await;
            let buffer_bytes = destination.repository().limits().read_buffer_bytes.max(1);
            let _memory = transfer_budget.reserve(buffer_bytes).await;
            let mut buffer = vec![0u8; buffer_bytes];
            loop {
                let read = reader.read(&mut buffer).await.map_err(|error| {
                    TransferError::SourcePayload {
                        key: record.key().clone(),
                        error: error.into(),
                    }
                })?;
                if read == 0 {
                    break;
                }
                writer
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|error| TransferError::Destination(RepositoryError::Io(error)))?;
            }
            let (actual, _) = writer.close().await.map_err(RepositoryError::Payload)?;
            if actual != payload {
                return Err(TransferError::Destination(
                    RepositoryError::PayloadIdentityMismatch {
                        expected: payload,
                        actual,
                    },
                ));
            }
            progress.payloads_sent += 1;
            if let Some(stats) = batched {
                progress.slice_copy_bytes += stats.copy_bytes;
                progress.slice_literal_bytes += stats.literal_bytes;
            }
            tracing::debug!("transferred and verified complete payload stream");
            Ok(progress)
        })
        .await
}

#[cfg(test)]
mod tests;
