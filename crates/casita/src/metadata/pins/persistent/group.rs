//! Process-local group commit. The kernel ledger lock still orders all processes.
use super::*;
use std::collections::VecDeque;
use std::sync::{
    Mutex, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
};

pub(super) const MAX_GROUP: usize = 64;
#[derive(Default)]
pub(super) struct Stats {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub frames: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub cached_edits: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub inventory_copies: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub inventory_diffs: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub bytes: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub syncs: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub checkpoints: AtomicU64,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub adoptions: AtomicU64,
    pub groups: AtomicU64,
    pub operations: AtomicU64,
    pub max_group: AtomicU64,
    pub replacements: AtomicU64,
}
impl Stats {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
        [
            ("journal_frames", &self.frames),
            ("cached_edits", &self.cached_edits),
            ("inventory_copies", &self.inventory_copies),
            ("inventory_diffs", &self.inventory_diffs),
            ("journal_bytes", &self.bytes),
            ("journal_syncs", &self.syncs),
            ("checkpoints", &self.checkpoints),
            ("journal_adoptions", &self.adoptions),
            ("groups", &self.groups),
            ("operations", &self.operations),
            ("max_group", &self.max_group),
            ("replacement_updates", &self.replacements),
        ]
        .into_iter()
        .map(|(name, value)| (name, value.load(Ordering::Relaxed)))
        .collect()
    }
}
struct Request {
    store: FilePinStore,
    operation: Operation,
    reply: tokio::sync::oneshot::Sender<Result<Outcome, MetadataError>>,
}
#[derive(Default)]
struct Queue {
    running: bool,
    pending: VecDeque<Request>,
}
#[derive(Default)]
pub(super) struct Local {
    queue: Mutex<Queue>,
    #[cfg(unix)]
    pub reader_cache: Mutex<Option<super::readers::CachedReaders>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub cache: Mutex<super::journal::Cache>,
    pub stats: Arc<Stats>,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub deny_growth: std::sync::atomic::AtomicBool,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub deny_exchange: std::sync::atomic::AtomicBool,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub pause: Mutex<Option<Pause>>,
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub fail: Mutex<Option<&'static str>>,
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(super) struct Pause {
    pub phase: &'static str,
    pub entered: std::sync::mpsc::Sender<()>,
    pub resume: std::sync::mpsc::Receiver<()>,
}
fn shared_error(error: &MetadataError) -> MetadataError {
    if matches!(error, MetadataError::StorageFull) {
        MetadataError::StorageFull
    } else {
        backend(error)
    }
}

impl FilePinStore {
    pub(super) fn local(&self) -> Result<Arc<Local>, MetadataError> {
        if let Some(local) = self.local.get() {
            return Ok(local.clone());
        }
        static REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Local>>>> = OnceLock::new();
        std::fs::create_dir_all(self.parent()).map_err(backend)?;
        let path = self.parent().canonicalize().map_err(backend)?.join(
            self.path
                .file_name()
                .ok_or_else(|| backend("missing ledger name"))?,
        );
        let mut registry = REGISTRY
            .get_or_init(Default::default)
            .lock()
            .map_err(backend)?;
        registry.retain(|_, local| local.strong_count() != 0);
        let local = registry
            .get(&path)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let local = Arc::new(Local::default());
                registry.insert(path, Arc::downgrade(&local));
                local
            });
        let _ = self.local.set(local.clone());
        Ok(self.local.get().unwrap().clone())
    }

    pub(super) async fn grouped_edit(
        &self,
        operation: Operation,
    ) -> Result<Outcome, MetadataError> {
        let _wait = LedgerPhase::new("group_wait");
        let local = self.local()?;
        let (reply, result) = tokio::sync::oneshot::channel();
        let start = {
            let mut queue = local.queue.lock().map_err(backend)?;
            queue.pending.push_back(Request {
                store: self.clone(),
                operation,
                reply,
            });
            let start = !queue.running;
            queue.running = true;
            start
        };
        if start {
            tokio::task::spawn_blocking(move || local.run());
        }
        result
            .await
            .map_err(|_| backend("local ledger worker stopped"))?
    }

    pub(super) fn edit_group(
        &self,
        operations: &[Operation],
    ) -> Result<Vec<Result<Outcome, MetadataError>>, MetadataError> {
        if operations.is_empty() || operations.len() > MAX_GROUP {
            return Err(backend("invalid ledger group size"));
        }
        let local = self.local()?;
        let _total = LedgerPhase::new("group_total");
        let lock = self.lock_file()?;
        let wait = LedgerPhase::new("exclusive_lock_wait");
        lock.lock().map_err(backend)?;
        drop(wait);
        // Invalidate tentative cache state before releasing the kernel lock on
        // any error or panic. Every reply waits for the final durability barrier.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut results = Vec::with_capacity(operations.len());
            for operation in operations {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Some(result) = self.edit_cached(operation)? {
                    results.push(Ok(result));
                    continue;
                }
                results.push(Ok(self.edit_with_readers(operation.clone(), true)?));
            }
            self.flush_pending_journal()?;
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            self.ledger_checkpoint("group-before-reply")?;
            Ok(results)
        }))
        .unwrap_or_else(|_| Err(backend("local ledger transaction panicked")));
        if result.is_err() {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                *local
                    .cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Default::default();
            }
        }
        lock.unlock().map_err(backend)?;
        local.stats.groups.fetch_add(1, Ordering::Relaxed);
        local
            .stats
            .operations
            .fetch_add(operations.len() as u64, Ordering::Relaxed);
        local
            .stats
            .max_group
            .fetch_max(operations.len() as u64, Ordering::Relaxed);
        result
    }
}

impl Local {
    fn run(self: Arc<Self>) {
        loop {
            let requests = {
                let mut queue = self.queue.lock().unwrap();
                if queue.pending.is_empty() {
                    queue.running = false;
                    return;
                }
                let count = queue.pending.len().min(MAX_GROUP);
                queue.pending.drain(..count).collect::<Vec<_>>()
            };
            let operations = requests
                .iter()
                .map(|request| request.operation.clone())
                .collect::<Vec<_>>();
            match requests[0].store.edit_group(&operations) {
                Ok(results) => {
                    for (request, result) in requests.into_iter().zip(results) {
                        let _ = request.reply.send(result);
                    }
                }
                Err(error) => {
                    for request in requests {
                        let _ = request.reply.send(Err(shared_error(&error)));
                    }
                }
            }
        }
    }
}
