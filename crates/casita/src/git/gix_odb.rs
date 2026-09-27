//! Synchronous Gix object traits backed by Casita's verified object repository.
//!
//! Gix deliberately exposes synchronous object-database traits. This adapter
//! keeps one Tokio runtime on a dedicated worker thread and crosses that
//! boundary through bounded channels. It is a compatibility layer: Git object
//! bodies remain ordinary Casita payloads and all identity, link extraction,
//! publication, retention, and collection semantics remain Casita-owned.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel as mpsc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::git::{
    GIT_SHA1_BLOB_NAMESPACE, GIT_SHA1_COMMIT_NAMESPACE, GIT_SHA1_TAG_NAMESPACE,
    GIT_SHA1_TREE_NAMESPACE, GIT_SHA256_BLOB_NAMESPACE, GIT_SHA256_COMMIT_NAMESPACE,
    GIT_SHA256_TAG_NAMESPACE, GIT_SHA256_TREE_NAMESPACE, GitError, GitObjectFormat, GitObjectKind,
    GitViewBody, git_object_key,
};
use crate::metadata::{MetadataStore, RootChange};
use crate::repository::{
    ConditionalPublishResult, MutationSession, OwnedRetentionHold, Repository, RepositoryError,
    RootExpectation, StagedObject,
};
use crate::{BlobStore, ObjectKey, ObjectRecord, RepositoryRevision, git_view_root_name};

/// Default number of objects published in one bounded mutation.
pub const DEFAULT_GIX_BATCH_OBJECTS: usize = 4096;
/// Default total body bytes that trigger a bounded publication.
pub const DEFAULT_GIX_BATCH_BYTES: u64 = 16 * 1024 * 1024;
/// Default number of committed OID payload summaries retained in the operation cache.
pub const DEFAULT_GIX_CACHE_OBJECTS: usize = 8192;
/// Default number of fixed-size messages allowed in each worker channel.
pub const DEFAULT_GIX_CHANNEL_CAPACITY: usize = 8;
/// Default chunk size used while bridging synchronous and asynchronous streams.
pub const DEFAULT_GIX_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Bounded resource policy for one [`CasitaGixOdb`] operation lifetime.
#[derive(Debug, Clone)]
pub struct CasitaGixOdbOptions {
    /// Native Git object hash used by this object database.
    pub object_format: GitObjectFormat,
    /// Publish after staging this many distinct objects.
    pub batch_objects: usize,
    /// Publish after staging at least this many object-body bytes.
    pub batch_bytes: u64,
    /// Maximum committed OID-to-payload-summary cache entries.
    pub cache_objects: usize,
    /// Maximum queued worker or stream messages.
    pub channel_capacity: usize,
    /// Maximum bytes carried by one streaming message.
    pub stream_chunk_bytes: usize,
}

impl CasitaGixOdbOptions {
    /// Production defaults for one native Git object format.
    pub fn new(object_format: GitObjectFormat) -> Self {
        Self {
            object_format,
            batch_objects: DEFAULT_GIX_BATCH_OBJECTS,
            batch_bytes: DEFAULT_GIX_BATCH_BYTES,
            cache_objects: DEFAULT_GIX_CACHE_OBJECTS,
            channel_capacity: DEFAULT_GIX_CHANNEL_CAPACITY,
            stream_chunk_bytes: DEFAULT_GIX_STREAM_CHUNK_BYTES,
        }
    }
}

/// Failure while adapting a synchronous Gix operation to Casita.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CasitaGixOdbError {
    /// A resource bound is zero or exceeds the repository's own bound.
    #[error("invalid Gix ODB options: {0}")]
    InvalidOptions(String),
    /// The supplied OID uses a different hash than this operation.
    #[error("Gix OID uses {actual:?}, but this Casita operation uses {expected:?}")]
    ObjectFormatMismatch {
        /// Hash selected when the adapter was opened.
        expected: GitObjectFormat,
        /// Hash carried by the Gix OID.
        actual: GitObjectFormat,
    },
    /// A stream ended before or after its exact declared size.
    #[error("declared {declared} object bytes, received {actual}")]
    SizeMismatch {
        /// Size supplied by Gix.
        declared: u64,
        /// Bytes actually consumed.
        actual: u64,
    },
    /// Gix supplied an identity that does not match the streamed body.
    #[error("known Git OID {expected} does not match computed OID {actual}")]
    KnownOidMismatch {
        /// OID supplied by Gix.
        expected: gix::ObjectId,
        /// OID reproduced from the exact kind, size, and body.
        actual: gix::ObjectId,
    },
    /// The synchronous input stream failed.
    #[error("Git object input failed: {0}")]
    Input(String),
    /// The worker stopped before completing the request.
    #[error("Casita Gix ODB worker is unavailable")]
    WorkerUnavailable,
    /// The worker thread could not be started.
    #[error("failed to start Casita Gix ODB worker: {0}")]
    WorkerStart(#[source] std::io::Error),
    /// The worker's Tokio runtime could not be constructed.
    #[error("failed to construct Casita Gix ODB runtime: {0}")]
    Runtime(#[source] std::io::Error),
    /// Casita rejected a native Git identity or body.
    #[error(transparent)]
    Git(#[from] GitError),
    /// Casita storage, verification, or publication failed.
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

type Result<T> = std::result::Result<T, CasitaGixOdbError>;

#[derive(Clone)]
struct CachedRecord {
    kind: gix::objs::Kind,
    key: ObjectKey,
    record: ObjectRecord,
}

#[derive(Clone, Copy)]
struct CachedPayload {
    kind: gix::objs::Kind,
    payload: crate::BlobId,
    size: u64,
}

impl CachedRecord {
    fn payload_summary(&self) -> CachedPayload {
        CachedPayload {
            kind: self.kind,
            payload: self.record.payload(),
            size: self.record.payload_size(),
        }
    }
}

struct RevisionCache {
    revision: RepositoryRevision,
    capacity: usize,
    entries: HashMap<gix::ObjectId, Option<CachedPayload>>,
    order: VecDeque<gix::ObjectId>,
}

struct HeaderCache {
    capacity: usize,
    entries: HashMap<gix::ObjectId, Option<(gix::objs::Kind, u64)>>,
    order: VecDeque<gix::ObjectId>,
}

impl HeaderCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, id: &gix::oid) -> Option<Option<(gix::objs::Kind, u64)>> {
        self.entries.get(id).copied()
    }

    fn insert(&mut self, id: gix::ObjectId, value: Option<(gix::objs::Kind, u64)>) {
        if self.entries.insert(id, value).is_none() {
            self.order.push_back(id);
        }
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn advance_revision(&mut self) {
        self.entries.retain(|_, header| header.is_some());
        self.order.retain(|id| self.entries.contains_key(id));
    }

    fn remove_all(&mut self, ids: &HashSet<gix::ObjectId>) {
        self.entries.retain(|id, _| !ids.contains(id));
        self.order.retain(|id| !ids.contains(id));
    }
}

impl RevisionCache {
    fn new(revision: RepositoryRevision, capacity: usize) -> Self {
        Self {
            revision,
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, id: &gix::oid) -> Option<Option<CachedPayload>> {
        self.entries.get(id).copied()
    }

    fn insert(&mut self, id: gix::ObjectId, value: Option<CachedPayload>) {
        if self.entries.insert(id, value).is_none() {
            self.order.push_back(id);
        }
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn advance_revision(&mut self, revision: RepositoryRevision) {
        self.revision = revision;
        self.entries.retain(|_, record| record.is_some());
        self.order.retain(|id| self.entries.contains_key(id));
    }
}

enum ReadEvent {
    Missing,
    Complete(gix::objs::Kind, Vec<u8>),
    Found(gix::objs::Kind, u64),
    Chunk(Vec<u8>),
    Error(CasitaGixOdbError),
}

enum InputEvent {
    Chunk(Vec<u8>),
    Error(String),
}

enum Request {
    Read {
        id: gix::ObjectId,
        events: mpsc::Sender<ReadEvent>,
    },
    Header {
        id: gix::ObjectId,
        reply: mpsc::Sender<Result<Option<(gix::objs::Kind, u64)>>>,
    },
    Write {
        kind: gix::objs::Kind,
        size: u64,
        known_id: Option<gix::ObjectId>,
        input: mpsc::Receiver<InputEvent>,
        reply: mpsc::Sender<Result<gix::ObjectId>>,
    },
    Flush {
        reply: mpsc::Sender<Result<()>>,
    },
    PublishView {
        payload: Vec<u8>,
        key: ObjectKey,
        root: crate::RootName,
        expected: Option<ObjectKey>,
        reply: mpsc::Sender<Result<ConditionalPublishResult>>,
    },
    Shutdown,
}

struct Inner {
    requests: mpsc::Sender<Request>,
    join: Mutex<Option<JoinHandle<()>>>,
    headers: Arc<Mutex<HeaderCache>>,
    options: CasitaGixOdbOptions,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
        if let Ok(join) = self.join.get_mut()
            && let Some(join) = join.take()
        {
            let _ = join.join();
        }
    }
}

/// A cloneable synchronous Gix object database backed by one Casita operation.
///
/// Writes become immediately visible through a staging overlay. Crossing an
/// object or byte threshold publishes an unrooted batch; [`flush`](Self::flush)
/// publishes the final partial batch. Dropping the last handle deliberately
/// does not flush, so a failed enclosing Git operation cannot accidentally
/// publish its incomplete tail.
#[derive(Clone)]
pub struct CasitaGixOdb {
    inner: Arc<Inner>,
}

impl CasitaGixOdb {
    /// Start an operation with the default bounded policy.
    pub fn new<PS, SS>(repository: Repository<PS, SS>, format: GitObjectFormat) -> Result<Self>
    where
        PS: BlobStore + Clone + 'static,
        SS: MetadataStore + Clone + 'static,
    {
        let mut options = CasitaGixOdbOptions::new(format);
        options.batch_objects = options
            .batch_objects
            .min(repository.limits().max_batch_objects);
        Self::with_options(repository, options)
    }

    /// Start an operation with explicit resource bounds.
    #[tracing::instrument(
        name = "git.odb.open",
        skip_all,
        fields(object_format = ?options.object_format)
    )]
    pub fn with_options<PS, SS>(
        repository: Repository<PS, SS>,
        options: CasitaGixOdbOptions,
    ) -> Result<Self>
    where
        PS: BlobStore + Clone + 'static,
        SS: MetadataStore + Clone + 'static,
    {
        validate_options(&repository, &options)?;
        let (requests, worker_requests) = mpsc::bounded(options.channel_capacity);
        let (ready, worker_ready) = mpsc::unbounded();
        let worker_options = options.clone();
        let worker_span = tracing::info_span!(
            parent: &tracing::Span::current(),
            "git.odb.worker",
            object_format = ?options.object_format
        );
        let headers = Arc::new(Mutex::new(HeaderCache::new(options.cache_objects)));
        let worker_headers = headers.clone();
        let join = std::thread::Builder::new()
            .name("casita-gix-odb".into())
            .spawn(move || {
                let _worker_span = worker_span.enter();
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(CasitaGixOdbError::Runtime(error)));
                        return;
                    }
                };
                runtime.block_on(worker(
                    repository,
                    worker_options,
                    worker_headers,
                    worker_requests,
                    ready,
                ));
            })
            .map_err(CasitaGixOdbError::WorkerStart)?;
        match worker_ready.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(Inner {
                    requests,
                    join: Mutex::new(Some(join)),
                    headers,
                    options,
                }),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(CasitaGixOdbError::WorkerUnavailable)
            }
        }
    }

    /// Native Git object format fixed for this operation.
    pub fn object_format(&self) -> GitObjectFormat {
        self.inner.options.object_format
    }

    /// Publish the final partial object batch and make its payloads durable.
    ///
    /// The enclosing Git operation should treat an error here as its own
    /// failure. This explicit boundary is intentionally not run from `Drop`.
    #[tracing::instrument(name = "git.odb.flush", skip_all)]
    pub fn flush(&self) -> Result<()> {
        let (reply, response) = mpsc::unbounded();
        self.send(Request::Flush { reply })?;
        response
            .recv()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
    }

    /// Atomically publish the pending tail and replace an immutable Git view
    /// if its currently selected object still matches `expected`.
    #[tracing::instrument(name = "git.odb.publish_view", skip_all)]
    pub fn publish_git_view(
        &self,
        view_name: &str,
        expected: Option<ObjectKey>,
        view: &GitViewBody,
    ) -> Result<ConditionalPublishResult> {
        if view.object_format != self.object_format() {
            return Err(CasitaGixOdbError::InvalidOptions(
                "Git view object format differs from the ODB operation".into(),
            ));
        }
        let root = git_view_root_name(view_name)
            .map_err(|error| CasitaGixOdbError::Input(error.to_string()))?;
        let payload = view.encode()?;
        let key = view.object_key()?;
        let (reply, response) = mpsc::unbounded();
        self.send(Request::PublishView {
            payload,
            key,
            root,
            expected,
            reply,
        })?;
        response
            .recv()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
    }

    /// Flush and consume this handle. Other clones, if any, remain usable.
    pub fn finish(self) -> Result<()> {
        self.flush()
    }

    fn send(&self, request: Request) -> Result<()> {
        self.inner
            .requests
            .send(request)
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)
    }

    fn checked_id(&self, id: &gix::oid) -> Result<gix::ObjectId> {
        let actual = casita_format(id.kind())?;
        let expected = self.object_format();
        if actual != expected {
            return Err(CasitaGixOdbError::ObjectFormatMismatch { expected, actual });
        }
        Ok(id.to_owned())
    }

    fn write_input(
        &self,
        kind: gix::objs::Kind,
        size: u64,
        from: &mut dyn Read,
        known_id: Option<gix::ObjectId>,
    ) -> Result<gix::ObjectId> {
        if let Some(id) = known_id.as_ref() {
            self.checked_id(id)?;
        }
        let (input, worker_input) = mpsc::bounded(self.inner.options.channel_capacity);
        let (reply, response) = mpsc::unbounded();
        self.send(Request::Write {
            kind,
            size,
            known_id,
            input: worker_input,
            reply,
        })?;

        let mut buffer = vec![0; self.inner.options.stream_chunk_bytes];
        loop {
            match from.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if input
                        .send(InputEvent::Chunk(buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = input.send(InputEvent::Error(error.to_string()));
                    break;
                }
            }
        }
        drop(input);
        response
            .recv()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
    }

    fn find_into<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> Result<Option<gix::objs::Data<'a>>> {
        let id = self.checked_id(id)?;
        let (events, worker_events) = mpsc::bounded(self.inner.options.channel_capacity);
        self.send(Request::Read { id, events })?;
        let first = worker_events
            .recv()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?;
        let kind = match first {
            ReadEvent::Missing => return Ok(None),
            ReadEvent::Complete(kind, bytes) => {
                buffer.clear();
                buffer.extend_from_slice(&bytes);
                return Ok(Some(gix::objs::Data {
                    kind,
                    object_hash: id.kind(),
                    data: buffer,
                }));
            }
            ReadEvent::Found(kind, size) => (kind, size),
            ReadEvent::Error(error) => return Err(error),
            ReadEvent::Chunk(_) => {
                return Err(CasitaGixOdbError::WorkerUnavailable);
            }
        };
        buffer.clear();
        while buffer.len() as u64 != kind.1 {
            match worker_events
                .recv()
                .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
            {
                ReadEvent::Chunk(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    if buffer.len() as u64 > kind.1 {
                        return Err(CasitaGixOdbError::WorkerUnavailable);
                    }
                }
                ReadEvent::Error(error) => return Err(error),
                ReadEvent::Missing | ReadEvent::Complete(_, _) | ReadEvent::Found(_, _) => {
                    return Err(CasitaGixOdbError::WorkerUnavailable);
                }
            }
        }
        Ok(Some(gix::objs::Data {
            kind: kind.0,
            object_hash: id.kind(),
            data: buffer,
        }))
    }

    fn header(&self, id: &gix::oid) -> Result<Option<(gix::objs::Kind, u64)>> {
        let id = self.checked_id(id)?;
        if let Some(header) = self
            .inner
            .headers
            .lock()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
            .get(&id)
        {
            return Ok(header);
        }
        let (reply, response) = mpsc::unbounded();
        self.send(Request::Header { id, reply })?;
        response
            .recv()
            .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
    }
}

impl gix::objs::Write for CasitaGixOdb {
    fn write_buf_with_known_id(
        &self,
        kind: gix::objs::Kind,
        from: &[u8],
        id: gix::ObjectId,
    ) -> std::result::Result<gix::ObjectId, gix::objs::write::Error> {
        self.write_input(kind, from.len() as u64, &mut &*from, Some(id))
            .map_err(Into::into)
    }

    fn write_stream(
        &self,
        kind: gix::objs::Kind,
        size: u64,
        from: &mut dyn Read,
    ) -> std::result::Result<gix::ObjectId, gix::objs::write::Error> {
        self.write_input(kind, size, from, None).map_err(Into::into)
    }

    fn write_stream_with_known_id(
        &self,
        kind: gix::objs::Kind,
        size: u64,
        from: &mut dyn Read,
        id: gix::ObjectId,
    ) -> std::result::Result<gix::ObjectId, gix::objs::write::Error> {
        self.write_input(kind, size, from, Some(id))
            .map_err(Into::into)
    }
}

impl gix::objs::Find for CasitaGixOdb {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> std::result::Result<Option<gix::objs::Data<'a>>, gix::objs::find::Error> {
        self.find_into(id, buffer).map_err(Into::into)
    }
}

impl gix::objs::FindHeader for CasitaGixOdb {
    fn try_header(
        &self,
        id: &gix::oid,
    ) -> std::result::Result<Option<gix::objs::Header>, gix::objs::find::Error> {
        Ok(self
            .header(id)?
            .map(|(kind, size)| gix::objs::Header { kind, size }))
    }
}

impl gix::objs::Exists for CasitaGixOdb {
    fn exists(&self, id: &gix::oid) -> bool {
        self.header(id).ok().flatten().is_some()
    }
}

impl gix::odb::Header for CasitaGixOdb {
    fn try_header(
        &self,
        id: &gix::oid,
    ) -> std::result::Result<Option<gix::odb::find::Header>, gix::objs::find::Error> {
        Ok(self
            .header(id)?
            .map(|(kind, size)| gix::odb::find::Header::Loose { kind, size }))
    }
}

struct WorkerState<PS, SS> {
    hold: OwnedRetentionHold<PS, SS>,
    cache: RevisionCache,
    headers: Arc<Mutex<HeaderCache>>,
    overlay: HashMap<gix::ObjectId, CachedRecord>,
}

async fn worker<PS, SS>(
    repository: Repository<PS, SS>,
    options: CasitaGixOdbOptions,
    headers: Arc<Mutex<HeaderCache>>,
    requests: mpsc::Receiver<Request>,
    ready: mpsc::Sender<Result<()>>,
) where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    let hold = match repository.owned_retention_hold().await {
        Ok(hold) => hold,
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return;
        }
    };
    let mutation = match repository.mutation_session().await {
        Ok(mutation) => mutation,
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return;
        }
    };
    let revision = hold.snapshot().revision();
    let mut state = WorkerState {
        hold,
        cache: RevisionCache::new(revision, options.cache_objects),
        headers,
        overlay: HashMap::new(),
    };
    let mut pending = Vec::new();
    let mut pending_bytes = 0u64;
    if ready.send(Ok(())).is_err() {
        return;
    }

    while let Ok(request) = requests.recv() {
        match request {
            Request::Read { id, events } => {
                stream_read(&repository, &options, &mut state, id, events).await;
            }
            Request::Header { id, reply } => {
                let _ = reply.send(resolve_header(&options, &mut state, id).await);
            }
            Request::Write {
                kind,
                size,
                known_id,
                input,
                reply,
            } => {
                let result = write_object(
                    &repository,
                    &mutation,
                    &options,
                    &mut state,
                    &mut pending,
                    &mut pending_bytes,
                    kind,
                    size,
                    known_id,
                    input,
                )
                .await;
                let _ = reply.send(result);
            }
            Request::Flush { reply } => {
                let result = publish_pending(
                    &repository,
                    &mutation,
                    &mut state,
                    &mut pending,
                    &mut pending_bytes,
                )
                .await;
                let _ = reply.send(result);
            }
            Request::PublishView {
                payload,
                key,
                root,
                expected,
                reply,
            } => {
                let result = publish_view(
                    &repository,
                    &mutation,
                    &mut state,
                    &mut pending,
                    &mut pending_bytes,
                    payload,
                    key,
                    root,
                    expected,
                )
                .await;
                let _ = reply.send(result);
            }
            Request::Shutdown => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_object<'a, PS, SS>(
    repository: &Repository<PS, SS>,
    mutation: &'a MutationSession<'_, PS, SS>,
    options: &CasitaGixOdbOptions,
    state: &mut WorkerState<PS, SS>,
    pending: &mut Vec<StagedObject<'a>>,
    pending_bytes: &mut u64,
    kind: gix::objs::Kind,
    declared_size: u64,
    known_id: Option<gix::ObjectId>,
    input: mpsc::Receiver<InputEvent>,
) -> Result<gix::ObjectId>
where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    let hash_kind = gix_hash_kind(options.object_format);
    let mut hasher = gix::hash::hasher(hash_kind);
    hasher.update(&gix::objs::encode::loose_header(kind, declared_size));
    let mut writer = repository.payloads().open_write().await;
    let mut actual_size = 0u64;
    while let Ok(event) = input.recv() {
        let bytes = match event {
            InputEvent::Chunk(bytes) => bytes,
            InputEvent::Error(error) => return Err(CasitaGixOdbError::Input(error)),
        };
        actual_size =
            actual_size
                .checked_add(bytes.len() as u64)
                .ok_or(CasitaGixOdbError::SizeMismatch {
                    declared: declared_size,
                    actual: u64::MAX,
                })?;
        if actual_size > declared_size {
            return Err(CasitaGixOdbError::SizeMismatch {
                declared: declared_size,
                actual: actual_size,
            });
        }
        hasher.update(&bytes);
        writer
            .write_all(&bytes)
            .await
            .map_err(RepositoryError::Io)?;
    }
    if actual_size != declared_size {
        return Err(CasitaGixOdbError::SizeMismatch {
            declared: declared_size,
            actual: actual_size,
        });
    }
    let (payload, stored_size) = writer.close().await.map_err(RepositoryError::Payload)?;
    if stored_size != actual_size {
        return Err(CasitaGixOdbError::SizeMismatch {
            declared: actual_size,
            actual: stored_size,
        });
    }
    let id = hasher
        .try_finalize()
        .map_err(|error| CasitaGixOdbError::Input(error.to_string()))?;
    if let Some(expected) = known_id
        && expected != id
    {
        return Err(CasitaGixOdbError::KnownOidMismatch {
            expected,
            actual: id,
        });
    }
    let key = git_object_key(
        options.object_format,
        casita_kind(kind),
        id.as_bytes().to_vec(),
    )?;
    let staged = mutation.stage_existing(key.clone(), payload).await?;
    let cached = CachedRecord {
        kind,
        key,
        record: staged.record().clone(),
    };
    if let Some(existing) = state.overlay.get(&id) {
        if existing.key != cached.key || existing.record != cached.record {
            return Err(CasitaGixOdbError::Input(format!(
                "conflicting staged records share Git OID {id}"
            )));
        }
        return Ok(id);
    }
    state.overlay.insert(id, cached);
    cache_header(state, id, Some((kind, actual_size)));
    pending.push(staged);
    *pending_bytes = pending_bytes
        .checked_add(actual_size)
        .ok_or_else(|| CasitaGixOdbError::InvalidOptions("pending byte count overflowed".into()))?;
    if pending.len() >= options.batch_objects || *pending_bytes >= options.batch_bytes {
        publish_pending(repository, mutation, state, pending, pending_bytes).await?;
    }
    Ok(id)
}

async fn publish_pending<'a, PS, SS>(
    repository: &Repository<PS, SS>,
    mutation: &'a MutationSession<'_, PS, SS>,
    state: &mut WorkerState<PS, SS>,
    pending: &mut Vec<StagedObject<'a>>,
    pending_bytes: &mut u64,
) -> Result<()>
where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    let objects = pending.len();
    let bytes = *pending_bytes;
    if pending.is_empty() {
        repository
            .payloads()
            .publication()
            .flush()
            .await
            .map_err(RepositoryError::Payload)?;
        return Ok(());
    }
    let publication = mutation.publish_unrooted(std::mem::take(pending)).await;
    *pending_bytes = 0;
    match publication {
        Ok(_) => {
            tracing::debug!(objects, bytes, "Git ODB batch published");
            refresh_hold(repository, state).await?;
            for (id, record) in state.overlay.drain() {
                let header = (record.kind, record.record.payload_size());
                state.cache.insert(id, Some(record.payload_summary()));
                if let Ok(mut headers) = state.headers.lock() {
                    headers.insert(id, Some(header));
                }
            }
            Ok(())
        }
        Err(error) => {
            discard_overlay(state);
            Err(error.into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn publish_view<'a, PS, SS>(
    repository: &Repository<PS, SS>,
    mutation: &'a MutationSession<'_, PS, SS>,
    state: &mut WorkerState<PS, SS>,
    pending: &mut Vec<StagedObject<'a>>,
    pending_bytes: &mut u64,
    payload: Vec<u8>,
    key: ObjectKey,
    root: crate::RootName,
    expected: Option<ObjectKey>,
) -> Result<ConditionalPublishResult>
where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    let decoded = GitViewBody::decode(&payload)?;
    validate_view_inventory(state, &decoded).await?;
    let view = mutation.stage_object(key.clone(), &payload).await?;
    let mut staged = std::mem::take(pending);
    staged.push(view);
    let publication = mutation
        .publish_if_roots_match(
            staged,
            vec![RootExpectation {
                name: root.clone(),
                target: expected,
            }],
            vec![RootChange::Set {
                name: root,
                target: key,
            }],
        )
        .await;
    *pending_bytes = 0;
    let result = match publication {
        Ok(result) => result,
        Err(error) => {
            discard_overlay(state);
            return Err(error.into());
        }
    };
    if matches!(&result, ConditionalPublishResult::Committed(_)) {
        refresh_hold(repository, state).await?;
        for (id, record) in state.overlay.drain() {
            let header = (record.kind, record.record.payload_size());
            state.cache.insert(id, Some(record.payload_summary()));
            if let Ok(mut headers) = state.headers.lock() {
                headers.insert(id, Some(header));
            }
        }
    } else {
        discard_overlay(state);
    }
    Ok(result)
}

async fn validate_view_inventory<PS, SS>(
    state: &WorkerState<PS, SS>,
    view: &GitViewBody,
) -> Result<()>
where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    const LOOKUP_BATCH: usize = 1_024;

    let mut reachable = view.direct_targets();
    let mut queue: VecDeque<_> = reachable.iter().cloned().collect();
    while !queue.is_empty() {
        let mut keys = Vec::with_capacity(LOOKUP_BATCH.min(queue.len()));
        while keys.len() < LOOKUP_BATCH {
            let Some(key) = queue.pop_front() else {
                break;
            };
            keys.push(key);
        }

        let mut records = vec![None; keys.len()];
        let mut committed_keys = Vec::new();
        let mut committed_indexes = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let (_, _, oid) = crate::git::git_key_parts(key)?;
            let id = gix::ObjectId::from_bytes_or_panic(oid);
            if let Some(cached) = state.overlay.get(&id).filter(|cached| cached.key == *key) {
                records[index] = Some(cached.record.clone());
            } else {
                committed_keys.push(key.clone());
                committed_indexes.push(index);
            }
        }
        let committed = state
            .hold
            .snapshot()
            .object_batch(&committed_keys)
            .await
            .map_err(RepositoryError::Metadata)?;
        for (index, record) in committed_indexes.into_iter().zip(committed) {
            records[index] = record;
        }

        for (key, record) in keys.into_iter().zip(records) {
            let record = record.ok_or_else(|| RepositoryError::Absent(key.to_string()))?;
            for link in record.links() {
                if !view.objects().contains(link) {
                    return Err(GitError::InvalidView(format!(
                        "reachable object {link} is absent from the object inventory"
                    ))
                    .into());
                }
                if reachable.insert(link.clone()) {
                    queue.push_back(link.clone());
                }
            }
        }
    }
    if let Some(extra) = view.objects().iter().find(|key| !reachable.contains(*key)) {
        return Err(GitError::InvalidView(format!(
            "object inventory contains unreachable object {extra}"
        ))
        .into());
    }
    Ok(())
}

fn discard_overlay<PS, SS>(state: &mut WorkerState<PS, SS>) {
    let discarded = state.overlay.keys().copied().collect::<HashSet<_>>();
    state.overlay.clear();
    if let Ok(mut headers) = state.headers.lock() {
        headers.remove_all(&discarded);
    }
}

async fn refresh_hold<PS, SS>(
    repository: &Repository<PS, SS>,
    state: &mut WorkerState<PS, SS>,
) -> Result<()>
where
    PS: BlobStore + Clone + 'static,
    SS: MetadataStore + Clone + 'static,
{
    let hold = repository.owned_retention_hold().await?;
    let revision = hold.snapshot().revision();
    state.hold = hold;
    state.cache.advance_revision(revision);
    state
        .headers
        .lock()
        .map_err(|_| CasitaGixOdbError::WorkerUnavailable)?
        .advance_revision();
    Ok(())
}

fn cache_header<PS, SS>(
    state: &WorkerState<PS, SS>,
    id: gix::ObjectId,
    header: Option<(gix::objs::Kind, u64)>,
) {
    if let Ok(mut headers) = state.headers.lock() {
        headers.insert(id, header);
    }
}

async fn resolve_header<PS, SS>(
    options: &CasitaGixOdbOptions,
    state: &mut WorkerState<PS, SS>,
    id: gix::ObjectId,
) -> Result<Option<(gix::objs::Kind, u64)>>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    Ok(resolve_payload(options, state, id)
        .await?
        .map(|payload| (payload.kind, payload.size)))
}

async fn resolve_payload<PS, SS>(
    options: &CasitaGixOdbOptions,
    state: &mut WorkerState<PS, SS>,
    id: gix::ObjectId,
) -> Result<Option<CachedPayload>>
where
    PS: BlobStore,
    SS: MetadataStore,
{
    if let Some(record) = state.overlay.get(&id) {
        let payload = record.payload_summary();
        cache_header(state, id, Some((payload.kind, payload.size)));
        return Ok(Some(payload));
    }
    if let Some(record) = state.cache.get(&id) {
        cache_header(
            state,
            id,
            record.as_ref().map(|record| (record.kind, record.size)),
        );
        return Ok(record);
    }
    let (key, (payload, size)) = match crate::git::resolve_git_oid_payload(
        state.hold.snapshot(),
        options.object_format,
        id.as_bytes(),
    )
    .await
    {
        Ok(found) => found,
        Err(GitError::MissingOid(_)) => {
            state.cache.insert(id, None);
            cache_header(state, id, None);
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let cached = CachedPayload {
        kind: gix_kind(&key)?,
        payload,
        size,
    };
    state.cache.insert(id, Some(cached));
    cache_header(state, id, Some((cached.kind, cached.size)));
    Ok(Some(cached))
}

async fn stream_read<PS, SS>(
    repository: &Repository<PS, SS>,
    options: &CasitaGixOdbOptions,
    state: &mut WorkerState<PS, SS>,
    id: gix::ObjectId,
    events: mpsc::Sender<ReadEvent>,
) where
    PS: BlobStore,
    SS: MetadataStore,
{
    let cached = match resolve_payload(options, state, id).await {
        Ok(Some(cached)) => cached,
        Ok(None) => {
            let _ = events.send(ReadEvent::Missing);
            return;
        }
        Err(error) => {
            let _ = events.send(ReadEvent::Error(error));
            return;
        }
    };
    let mut reader = match repository.payloads().open_read(&cached.payload).await {
        Ok(Some(reader)) => reader,
        Ok(None) => {
            let _ = events.send(ReadEvent::Error(
                RepositoryError::MissingPayload(cached.payload).into(),
            ));
            return;
        }
        Err(error) => {
            let _ = events.send(ReadEvent::Error(RepositoryError::Payload(error).into()));
            return;
        }
    };
    let size = cached.size;
    if size <= options.stream_chunk_bytes as u64 {
        let mut bytes = vec![0; size as usize];
        if let Err(error) = reader.read_exact(&mut bytes).await {
            let _ = events.send(ReadEvent::Error(RepositoryError::Io(error).into()));
            return;
        }
        let _ = events.send(ReadEvent::Complete(cached.kind, bytes));
        return;
    }
    if events.send(ReadEvent::Found(cached.kind, size)).is_err() {
        return;
    }
    let mut remaining = size;
    let mut buffer = vec![0; options.stream_chunk_bytes];
    while remaining != 0 {
        let limit = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        match reader.read(&mut buffer[..limit]).await {
            Ok(0) => {
                let _ = events.send(ReadEvent::Error(
                    RepositoryError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "payload {} ended after {} of {size} bytes",
                            cached.payload,
                            size - remaining
                        ),
                    ))
                    .into(),
                ));
                return;
            }
            Ok(read) => {
                remaining -= read as u64;
                if events
                    .send(ReadEvent::Chunk(buffer[..read].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = events.send(ReadEvent::Error(RepositoryError::Io(error).into()));
                return;
            }
        }
    }
}

fn validate_options<PS, SS>(
    repository: &Repository<PS, SS>,
    options: &CasitaGixOdbOptions,
) -> Result<()> {
    if options.batch_objects == 0
        || options.batch_bytes == 0
        || options.cache_objects == 0
        || options.channel_capacity == 0
        || options.stream_chunk_bytes == 0
    {
        return Err(CasitaGixOdbError::InvalidOptions(
            "all resource bounds must be nonzero".into(),
        ));
    }
    if options.batch_objects > repository.limits().max_batch_objects {
        return Err(CasitaGixOdbError::InvalidOptions(format!(
            "batch_objects {} exceeds repository limit {}",
            options.batch_objects,
            repository.limits().max_batch_objects
        )));
    }
    Ok(())
}

fn gix_hash_kind(format: GitObjectFormat) -> gix::hash::Kind {
    match format {
        GitObjectFormat::Sha1 => gix::hash::Kind::Sha1,
        GitObjectFormat::Sha256 => gix::hash::Kind::Sha256,
    }
}

fn casita_format(format: gix::hash::Kind) -> Result<GitObjectFormat> {
    match format {
        gix::hash::Kind::Sha1 => Ok(GitObjectFormat::Sha1),
        gix::hash::Kind::Sha256 => Ok(GitObjectFormat::Sha256),
        _ => Err(CasitaGixOdbError::Input(
            "Gix supplied an unsupported object hash".into(),
        )),
    }
}

fn casita_kind(kind: gix::objs::Kind) -> GitObjectKind {
    match kind {
        gix::objs::Kind::Blob => GitObjectKind::Blob,
        gix::objs::Kind::Tree => GitObjectKind::Tree,
        gix::objs::Kind::Commit => GitObjectKind::Commit,
        gix::objs::Kind::Tag => GitObjectKind::Tag,
    }
}

fn gix_kind(key: &ObjectKey) -> Result<gix::objs::Kind> {
    match key.namespace().as_str() {
        GIT_SHA1_BLOB_NAMESPACE | GIT_SHA256_BLOB_NAMESPACE => Ok(gix::objs::Kind::Blob),
        GIT_SHA1_TREE_NAMESPACE | GIT_SHA256_TREE_NAMESPACE => Ok(gix::objs::Kind::Tree),
        GIT_SHA1_COMMIT_NAMESPACE | GIT_SHA256_COMMIT_NAMESPACE => Ok(gix::objs::Kind::Commit),
        GIT_SHA1_TAG_NAMESPACE | GIT_SHA256_TAG_NAMESPACE => Ok(gix::objs::Kind::Tag),
        namespace => Err(CasitaGixOdbError::Input(format!(
            "resolved non-Git namespace {namespace}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use std::collections::BTreeMap;
    use std::io;
    use std::sync::{Arc, Barrier, Mutex};

    use gix::objs::{Find, FindHeader, Write};

    use super::*;
    use crate::{CanonicalRefName, GitRefValue};

    fn options(format: GitObjectFormat) -> CasitaGixOdbOptions {
        CasitaGixOdbOptions {
            object_format: format,
            batch_objects: 32,
            batch_bytes: 1024 * 1024,
            cache_objects: 4,
            channel_capacity: 2,
            stream_chunk_bytes: 4096,
        }
    }

    fn object_id(format: GitObjectFormat, kind: gix::objs::Kind, body: &[u8]) -> gix::ObjectId {
        gix::objs::compute_hash(gix_hash_kind(format), kind, body).unwrap()
    }

    #[test]
    fn production_defaults_bound_the_standard_oid_working_set() {
        let policy = CasitaGixOdbOptions::new(GitObjectFormat::Sha1);
        assert_eq!(policy.cache_objects, 8192);
        assert_eq!(policy.cache_objects, DEFAULT_GIX_CACHE_OBJECTS);
    }

    fn commit_body(tree: gix::ObjectId, message: &str) -> Vec<u8> {
        format!(
            "tree {tree}\nauthor Casita <casita@invalid> 1700000000 +0000\ncommitter Casita <casita@invalid> 1700000000 +0000\n\n{message}\n"
        )
        .into_bytes()
    }

    fn direct_view(
        format: GitObjectFormat,
        tree: gix::ObjectId,
        target: gix::ObjectId,
    ) -> GitViewBody {
        let name = CanonicalRefName::try_from("refs/heads/main").unwrap();
        let target =
            git_object_key(format, GitObjectKind::Commit, target.as_bytes().to_vec()).unwrap();
        let tree = git_object_key(format, GitObjectKind::Tree, tree.as_bytes().to_vec()).unwrap();
        GitViewBody {
            object_format: format,
            refs: BTreeMap::from([(name.clone(), GitRefValue::Direct(target.clone()))]),
            default_ref: Some(name),
            pack: None,
            objects: BTreeSet::from([tree, target]),
        }
    }

    #[test]
    fn every_kind_round_trips_before_flush_and_after_reopen_for_both_hashes() {
        for format in [GitObjectFormat::Sha1, GitObjectFormat::Sha256] {
            let repository = Repository::memory().unwrap();
            let odb = CasitaGixOdb::with_options(repository.clone(), options(format)).unwrap();
            let blob = b"native blob body".to_vec();
            let blob_id = object_id(format, gix::objs::Kind::Blob, &blob);
            let mut tree = b"100644 file\0".to_vec();
            tree.extend_from_slice(blob_id.as_bytes());
            let tree_id = object_id(format, gix::objs::Kind::Tree, &tree);
            let commit = commit_body(tree_id, "round trip");
            let commit_id = object_id(format, gix::objs::Kind::Commit, &commit);
            let tag = format!(
                "object {commit_id}\ntype commit\ntag v1\ntagger Casita <casita@invalid> 1700000000 +0000\n\nrelease\n"
            )
            .into_bytes();
            let objects = [
                (gix::objs::Kind::Blob, blob_id, blob),
                (gix::objs::Kind::Tree, tree_id, tree),
                (gix::objs::Kind::Commit, commit_id, commit),
                (
                    gix::objs::Kind::Tag,
                    object_id(format, gix::objs::Kind::Tag, &tag),
                    tag,
                ),
            ];

            let mut buffer = Vec::new();
            for (kind, id, body) in &objects {
                assert_eq!(odb.write_buf(*kind, body).unwrap(), *id);
                let found = odb.try_find(id, &mut buffer).unwrap().unwrap();
                assert_eq!(found.kind, *kind);
                assert_eq!(found.data, body);
                assert_eq!(
                    FindHeader::try_header(&odb, id).unwrap(),
                    Some(gix::objs::Header {
                        kind: *kind,
                        size: body.len() as u64,
                    })
                );
            }
            odb.flush().unwrap();
            drop(odb);

            let reopened = CasitaGixOdb::with_options(repository, options(format)).unwrap();
            for (kind, id, body) in &objects {
                let found = reopened.try_find(id, &mut buffer).unwrap().unwrap();
                assert_eq!(found.kind, *kind);
                assert_eq!(found.data, body);
            }
        }
    }

    #[test]
    fn rejects_a_wrong_known_oid() {
        for format in [GitObjectFormat::Sha1, GitObjectFormat::Sha256] {
            let repository = Repository::memory().unwrap();
            let odb = CasitaGixOdb::with_options(repository, options(format)).unwrap();
            let wrong = gix::ObjectId::from_bytes_or_panic(&vec![7; format.oid_len()]);
            let error = odb
                .write_buf_with_known_id(gix::objs::Kind::Blob, b"body", wrong)
                .unwrap_err();
            assert!(error.to_string().contains("does not match computed OID"));
            assert!(FindHeader::try_header(&odb, &wrong).unwrap().is_none());
        }
    }

    struct BoundedReader {
        remaining: usize,
        max_request: Arc<Mutex<usize>>,
    }

    impl Read for BoundedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut max_request = self.max_request.lock().unwrap();
            *max_request = (*max_request).max(buffer.len());
            drop(max_request);
            let read = self.remaining.min(buffer.len());
            buffer[..read].fill(b'x');
            self.remaining -= read;
            Ok(read)
        }
    }

    #[test]
    fn large_stream_uses_the_configured_bounded_chunks() {
        let repository = Repository::memory().unwrap();
        let mut policy = options(GitObjectFormat::Sha1);
        policy.stream_chunk_bytes = 1024;
        policy.batch_bytes = 8 * 1024 * 1024;
        let odb = CasitaGixOdb::with_options(repository, policy).unwrap();
        let size = 3 * 1024 * 1024;
        let max_request = Arc::new(Mutex::new(0));
        let mut reader = BoundedReader {
            remaining: size,
            max_request: max_request.clone(),
        };
        let id = odb
            .write_stream(gix::objs::Kind::Blob, size as u64, &mut reader)
            .unwrap();
        assert_eq!(*max_request.lock().unwrap(), 1024);
        assert_eq!(
            FindHeader::try_header(&odb, &id).unwrap().unwrap().size,
            size as u64
        );
    }

    #[test]
    fn interrupted_batch_keeps_published_progress_but_not_its_unflushed_tail() {
        let repository = Repository::memory().unwrap();
        let mut policy = options(GitObjectFormat::Sha1);
        policy.batch_objects = 2;
        let odb = CasitaGixOdb::with_options(repository.clone(), policy.clone()).unwrap();
        let first = odb
            .write_buf(gix::objs::Kind::Blob, b"published first")
            .unwrap();
        let second = odb
            .write_buf(gix::objs::Kind::Blob, b"published second")
            .unwrap();
        let tail = odb
            .write_buf(gix::objs::Kind::Blob, b"interrupted tail")
            .unwrap();
        assert!(FindHeader::try_header(&odb, &tail).unwrap().is_some());
        drop(odb);

        let reopened = CasitaGixOdb::with_options(repository, policy).unwrap();
        assert!(FindHeader::try_header(&reopened, &first).unwrap().is_some());
        assert!(
            FindHeader::try_header(&reopened, &second)
                .unwrap()
                .is_some()
        );
        assert!(FindHeader::try_header(&reopened, &tail).unwrap().is_none());
    }

    #[test]
    fn local_repository_closes_reopens_and_resolves_by_oid() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let repository = runtime.block_on(Repository::local(root.path())).unwrap();
        let odb = CasitaGixOdb::new(repository.clone(), GitObjectFormat::Sha1).unwrap();
        let id = odb
            .write_buf(gix::objs::Kind::Blob, b"persistent Git object")
            .unwrap();
        odb.flush().unwrap();
        drop(odb);
        drop(repository);

        let repository = runtime.block_on(Repository::local(root.path())).unwrap();
        let reopened = CasitaGixOdb::new(repository, GitObjectFormat::Sha1).unwrap();
        let mut buffer = Vec::new();
        assert_eq!(
            reopened.try_find(&id, &mut buffer).unwrap().unwrap().data,
            b"persistent Git object"
        );
    }

    #[test]
    fn local_profile_retains_just_sealed_bytes_in_the_bounded_pack_cache() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let repository = runtime.block_on(Repository::local(root.path())).unwrap();
        let odb = CasitaGixOdb::new(repository.clone(), GitObjectFormat::Sha1).unwrap();
        let id = odb
            .write_buf(gix::objs::Kind::Blob, b"cache this Git object")
            .unwrap();
        odb.flush().unwrap();
        repository.payloads().reset_pack_read_stats();

        let mut buffer = Vec::new();
        for _ in 0..3 {
            assert_eq!(
                odb.try_find(&id, &mut buffer).unwrap().unwrap().data,
                b"cache this Git object"
            );
        }
        let stats = repository.payloads().pack_read_stats().unwrap();
        assert_eq!(stats.chunk_range_requests, 0);
        assert_eq!(stats.whole_pack_requests, 0);
        assert_eq!(stats.cache_hits, 3);
    }

    #[test]
    fn stale_concurrent_view_compare_and_swap_loses_without_publishing() {
        let format = GitObjectFormat::Sha1;
        let repository = Repository::memory().unwrap();
        let setup = CasitaGixOdb::with_options(repository.clone(), options(format)).unwrap();
        let tree = Vec::new();
        let tree_id = setup.write_buf(gix::objs::Kind::Tree, &tree).unwrap();
        let initial_commit = commit_body(tree_id, "initial");
        let first_commit = commit_body(tree_id, "first");
        let second_commit = commit_body(tree_id, "second");
        let initial_id = setup
            .write_buf(gix::objs::Kind::Commit, &initial_commit)
            .unwrap();
        let first_id = setup
            .write_buf(gix::objs::Kind::Commit, &first_commit)
            .unwrap();
        let second_id = setup
            .write_buf(gix::objs::Kind::Commit, &second_commit)
            .unwrap();
        let initial_view = direct_view(format, tree_id, initial_id);
        assert!(matches!(
            setup
                .publish_git_view("origin", None, &initial_view)
                .unwrap(),
            ConditionalPublishResult::Committed(_)
        ));
        let initial_key = initial_view.object_key().unwrap();
        drop(setup);

        let first = CasitaGixOdb::with_options(repository.clone(), options(format)).unwrap();
        let second = CasitaGixOdb::with_options(repository, options(format)).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let run = |odb: CasitaGixOdb, view: GitViewBody, barrier: Arc<Barrier>| {
            let expected = initial_key.clone();
            std::thread::spawn(move || {
                barrier.wait();
                odb.publish_git_view("origin", Some(expected), &view)
                    .unwrap()
            })
        };
        let first = run(
            first,
            direct_view(format, tree_id, first_id),
            barrier.clone(),
        );
        let second = run(
            second,
            direct_view(format, tree_id, second_id),
            barrier.clone(),
        );
        barrier.wait();
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, ConditionalPublishResult::Committed(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, ConditionalPublishResult::RootMismatch { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn failed_view_compare_and_swap_discards_overlay_headers() {
        let format = GitObjectFormat::Sha1;
        let repository = Repository::memory().unwrap();
        let setup = CasitaGixOdb::with_options(repository.clone(), options(format)).unwrap();
        let tree_id = setup.write_buf(gix::objs::Kind::Tree, &[]).unwrap();
        let initial_id = setup
            .write_buf(gix::objs::Kind::Commit, &commit_body(tree_id, "initial"))
            .unwrap();
        let initial_view = direct_view(format, tree_id, initial_id);
        assert!(matches!(
            setup
                .publish_git_view("origin", None, &initial_view)
                .unwrap(),
            ConditionalPublishResult::Committed(_)
        ));
        let initial_key = initial_view.object_key().unwrap();
        drop(setup);

        let stale = CasitaGixOdb::with_options(repository.clone(), options(format)).unwrap();
        let advancing = CasitaGixOdb::with_options(repository, options(format)).unwrap();
        let losing_id = stale
            .write_buf(gix::objs::Kind::Commit, &commit_body(tree_id, "losing"))
            .unwrap();
        let winning_id = advancing
            .write_buf(gix::objs::Kind::Commit, &commit_body(tree_id, "winning"))
            .unwrap();
        assert!(matches!(
            advancing
                .publish_git_view(
                    "origin",
                    Some(initial_key.clone()),
                    &direct_view(format, tree_id, winning_id),
                )
                .unwrap(),
            ConditionalPublishResult::Committed(_)
        ));
        assert!(matches!(
            stale
                .publish_git_view(
                    "origin",
                    Some(initial_key),
                    &direct_view(format, tree_id, losing_id),
                )
                .unwrap(),
            ConditionalPublishResult::RootMismatch { .. }
        ));

        assert!(
            FindHeader::try_header(&stale, &losing_id)
                .unwrap()
                .is_none()
        );
        assert!(!gix::objs::Exists::exists(&stale, &losing_id));
        assert!(
            stale
                .try_find(&losing_id, &mut Vec::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn gix_decodes_commit_traversal_from_casita_only() {
        let repository = Repository::memory().unwrap();
        let writer = CasitaGixOdb::new(repository.clone(), GitObjectFormat::Sha1).unwrap();
        let blob_id = writer.write_buf(gix::objs::Kind::Blob, b"file").unwrap();
        let mut tree = b"100644 file\0".to_vec();
        tree.extend_from_slice(blob_id.as_bytes());
        let tree_id = writer.write_buf(gix::objs::Kind::Tree, &tree).unwrap();
        let commit = commit_body(tree_id, "traverse");
        let commit_id = writer.write_buf(gix::objs::Kind::Commit, &commit).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let reader = CasitaGixOdb::new(repository, GitObjectFormat::Sha1).unwrap();
        let mut commit_buffer = Vec::new();
        let commit = reader
            .try_find(&commit_id, &mut commit_buffer)
            .unwrap()
            .unwrap()
            .decode()
            .unwrap()
            .into_commit()
            .unwrap();
        assert_eq!(commit.tree(), tree_id);
        drop(commit);
        let mut tree_buffer = Vec::new();
        let tree = reader
            .try_find(&tree_id, &mut tree_buffer)
            .unwrap()
            .unwrap()
            .decode()
            .unwrap()
            .into_tree()
            .unwrap();
        let entry = tree.entries.first().unwrap();
        assert_eq!(entry.oid, blob_id);
        let mut blob_buffer = Vec::new();
        assert_eq!(
            reader
                .try_find(&blob_id, &mut blob_buffer)
                .unwrap()
                .unwrap()
                .data,
            b"file"
        );
    }
}
