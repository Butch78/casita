//! Repository backend and instrumentation for the native FSKit adapter.
use anyhow::{anyhow, ensure, Result};
use casita_fs::{ContentReader, ContentStream, FilesystemNode, FilesystemNodeKind, FilesystemView};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
    time::Instant,
};

mod config;
mod diagnostics;
mod directory_cache;
mod reader_cache;
use config::BackendConfig;
pub use diagnostics::Diagnostics;
use directory_cache::CachedDirectory;
use reader_cache::ReaderCache;

pub const READER_CACHE_CAPACITY: usize = 32;

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub digest: String,
    pub size: u64,
}
impl Snapshot {
    pub fn node(&self) -> Result<casita::Node> {
        Ok(casita::Node::Directory {
            digest: self.digest.parse()?,
            size: self.size,
        })
    }
}
pub struct Reader {
    pub repository: casita::Repository,
    pub counters: Diagnostics,
}
impl Reader {
    async fn open_stream(&self, key: &casita::BlobId) -> io::Result<Option<casita::Reader>> {
        self.counters.opens.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let result = self.repository.open(&casita::ObjectKey::blob(*key)).await;
        self.counters
            .open_ns
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        result.map_err(io::Error::other)
    }
}
#[async_trait::async_trait]
impl ContentReader for Reader {
    async fn directory(&self, key: &casita::DirectoryId) -> io::Result<Option<casita::Directory>> {
        self.counters.directories.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let result = self.repository.directory(key).await;
        self.counters
            .directory_ns
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        result
    }
    async fn open_blob(&self, key: &casita::BlobId) -> io::Result<Option<Box<dyn ContentStream>>> {
        Ok(self
            .open_stream(key)
            .await?
            .map(|reader| Box::new(reader) as Box<dyn ContentStream>))
    }
    async fn checkout_directory(&self, key: &casita::DirectoryId, path: &Path) -> io::Result<()> {
        self.repository.checkout_directory(key, path).await
    }
}
#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    pub parent: u64,
    pub name: Vec<u8>,
    pub node: Option<FilesystemNode>,
}
impl Entry {
    pub fn kind(&self) -> FilesystemNodeKind {
        self.node
            .as_ref()
            .map_or(FilesystemNodeKind::Directory, FilesystemNode::kind)
    }
    pub fn size(&self) -> u64 {
        self.node.as_ref().map_or(0, FilesystemNode::size)
    }
    pub fn mode(&self) -> u16 {
        if self.id == 3 {
            // Publication still requires a pre-staged immutable root descriptor.
            0o755
        } else if self.kind() == FilesystemNodeKind::Directory
            || self.node.as_ref().is_some_and(FilesystemNode::executable)
        {
            0o555
        } else {
            0o444
        }
    }
}
struct Nodes {
    entries: HashMap<u64, Entry>,
    children: HashMap<(u64, Vec<u8>), u64>,
    // Content-addressed directories cannot change. Cache the complete directory
    // rather than an unbounded set of names that callers have failed to find.
    directories: HashMap<u64, Arc<CachedDirectory>>,
    next: u64,
}
pub struct Backend {
    pub reader: Arc<Reader>,
    pub view: FilesystemView,
    pub path: PathBuf,
    nodes: Mutex<Nodes>,
    pub(crate) config: BackendConfig,
    production: bool,
    file_readers: ReaderCache,
    // Drop the runtime after repository/view owners and their cleanup tasks.
    pub runtime: tokio::runtime::Runtime,
}
impl Backend {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_at(path, None, BackendConfig::from_markers(path)?)
    }
    pub fn open_native(path: &Path) -> Result<Self> {
        let control = casita_fs::darwin::native_protocol::session(path)?;
        let config = BackendConfig::from_markers(&control)?;
        Self::open_at(path, Some(control), config)
    }
    fn open_at(
        repository_path: &Path,
        control: Option<PathBuf>,
        config: BackendConfig,
    ) -> Result<Self> {
        let production = control.is_some();
        let path = control.as_deref().unwrap_or(repository_path);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let repository = runtime.block_on(casita::Repository::local(repository_path))?;
        let roots = if production {
            BTreeMap::new()
        } else {
            let snapshot: Snapshot =
                serde_json::from_slice(&std::fs::read(path.join("snapshot.json"))?)?;
            [(b"fixture".to_vec(), snapshot.node()?)].into()
        };
        let reader = Arc::new(Reader {
            repository,
            counters: Diagnostics::default(),
        });
        let view = FilesystemView::new(reader.clone(), roots);
        let mut entries = vec![
            Entry {
                id: 2,
                parent: 1,
                name: vec![],
                node: None,
            },
            Entry {
                id: 3,
                parent: 2,
                name: b"views".to_vec(),
                node: None,
            },
        ];
        if !production {
            entries.push(Entry {
                id: 4,
                parent: 3,
                name: b"fixture".to_vec(),
                node: view.root(b"fixture"),
            });
        }
        let nodes = Nodes {
            children: entries
                .iter()
                .skip(1)
                .map(|e| ((e.parent, e.name.clone()), e.id))
                .collect(),
            entries: entries.into_iter().map(|e| (e.id, e)).collect(),
            directories: HashMap::new(),
            next: 5,
        };
        Ok(Self {
            runtime,
            reader,
            view,
            nodes: Mutex::new(nodes),
            config,
            production,
            file_readers: ReaderCache::new(config.reader_cache_capacity, config.volume.trace_reads),
            path: path.to_owned(),
        })
    }
    pub fn entry(&self, id: u64) -> Result<Entry> {
        self.nodes
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown inode"))
    }
    fn insert(&self, parent: u64, name: &[u8], node: FilesystemNode) -> Entry {
        let mut nodes = self.nodes.lock().unwrap();
        if let Some(id) = nodes.children.get(&(parent, name.to_vec())) {
            return nodes.entries[id].clone();
        }
        let entry = Entry {
            id: nodes.next,
            parent,
            name: name.to_vec(),
            node: Some(node),
        };
        nodes.next += 1;
        nodes.children.insert((parent, name.to_vec()), entry.id);
        nodes.entries.insert(entry.id, entry.clone());
        entry
    }
    fn directory(&self, id: u64, node: &FilesystemNode) -> Result<Arc<CachedDirectory>> {
        let cached = self.nodes.lock().unwrap().directories.get(&id).cloned();
        if let Some(cached) = cached {
            return Ok(cached);
        }
        let _entered = self.runtime.enter();
        let entries = self.runtime.block_on(self.view.entries(node))?;
        let entries = Arc::new(CachedDirectory::new(entries));
        if !self.config.cache_directories {
            return Ok(entries);
        }
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .directories
            .entry(id)
            .or_insert(entries)
            .clone())
    }
    pub fn lookup(&self, parent: u64, name: &[u8]) -> Result<Option<Entry>> {
        let cached = self
            .nodes
            .lock()
            .unwrap()
            .children
            .get(&(parent, name.to_vec()))
            .copied();
        if let Some(id) = cached {
            return self.entry(id).map(Some);
        }
        let directory = self.entry(parent)?;
        let Some(node) = directory.node else {
            return Ok(None);
        };
        let start = Instant::now();
        let found = self.directory(parent, &node)?.get(name).cloned();
        if found.is_none() {
            self.reader.counters.record_missing_lookup(
                parent,
                name,
                start.elapsed().as_nanos() as u64,
            );
        }
        Ok(found.map(|node| self.insert(parent, name, node)))
    }
    pub fn entries(&self, parent: u64) -> Result<Vec<Entry>> {
        let directory = self.entry(parent)?;
        if let Some(node) = directory.node {
            Ok(self
                .directory(parent, &node)?
                .iter()
                .map(|(name, node)| self.insert(parent, name, node.clone()))
                .collect())
        } else {
            let nodes = self.nodes.lock().unwrap();
            let mut entries: Vec<_> = nodes
                .entries
                .values()
                .filter(|e| e.parent == parent && e.id != parent)
                .cloned()
                .collect();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        }
    }
    /// Immutable directory metadata and the converted enumeration share one
    /// cache entry. Publication namespaces always produce a fresh snapshot.
    pub(crate) fn directory_snapshot(
        &self,
        parent: u64,
        build: impl FnOnce(Vec<Entry>) -> crate::filesystem::DirectorySnapshot,
    ) -> Result<crate::filesystem::DirectorySnapshot> {
        let entry = self.entry(parent)?;
        let Some(node) = entry.node else {
            return Ok(build(self.entries(parent)?));
        };
        let directory = self.directory(parent, &node)?;
        Ok(directory.snapshot(
            self.config.cache_directories && self.config.cache_enumeration,
            || {
                build(
                    directory
                        .iter()
                        .map(|(name, node)| self.insert(parent, name, node.clone()))
                        .collect(),
                )
            },
        ))
    }
    pub fn read(&self, id: u64, offset: u64, length: u32) -> Result<Vec<u8>> {
        if !self.config.volume.trace_reads {
            return self.read_inner(id, offset, length);
        }
        let started = Instant::now();
        let result = self.read_inner(id, offset, length);
        self.reader.counters.record_read_range(
            id,
            offset,
            length,
            result.as_ref().ok().map(Vec::len),
            started.elapsed().as_nanos() as u64,
        );
        result
    }
    fn read_inner(&self, id: u64, offset: u64, length: u32) -> Result<Vec<u8>> {
        let entry = self.entry(id)?;
        let node = entry.node.ok_or_else(|| anyhow!("not a file"))?;
        let _entered = self.runtime.enter();
        self.reader.counters.reads.fetch_add(1, Ordering::Relaxed);
        let bytes = if self.config.cache_readers {
            self.file_readers
                .read_range(&self.runtime, &self.reader, id, &node, offset, length)?
        } else {
            self.runtime
                .block_on(self.view.read(&node, offset, length))?
        };
        self.reader
            .counters
            .bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }
    pub fn staged_view(&self, name: &str) -> Result<FilesystemView> {
        ensure!(
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid publication name"
        );
        let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(
            self.path.join(format!("stage-{name}.json")),
        )?)?;
        Ok(FilesystemView::new(
            self.reader.clone(),
            [(name.as_bytes().to_vec(), snapshot.node()?)].into(),
        ))
    }
    pub fn publish(&self, name: &[u8]) -> Result<Entry> {
        self.publish_checked(name, FilesystemNodeKind::Directory, None)
    }
    pub fn publish_checked(
        &self,
        name: &[u8],
        kind: FilesystemNodeKind,
        target: Option<&[u8]>,
    ) -> Result<Entry> {
        ensure!(self.lookup(3, name)?.is_none(), "already published");
        let node = if self.production {
            FilesystemNode::from_casita(casita_fs::darwin::native_protocol::read(&self.path, name)?)
        } else {
            self.staged_view(std::str::from_utf8(name)?)?
                .root(name)
                .ok_or_else(|| anyhow!("missing staged root"))?
        };
        ensure!(node.kind() == kind, "publication type mismatch");
        ensure!(
            kind != FilesystemNodeKind::Symlink || node.symlink_target() == target,
            "publication symlink mismatch"
        );
        Ok(self.insert(3, name, node))
    }
    pub fn stats_file_size(&self) -> usize {
        Diagnostics::file_size(self.config.volume.trace_reads)
    }
    pub fn stats(&self) -> serde_json::Value {
        let files = if self.config.volume.trace_reads {
            self.nodes
                .lock()
                .unwrap()
                .entries
                .iter()
                .map(|(id, entry)| {
                    (
                        id.to_string(),
                        String::from_utf8_lossy(&entry.name).into_owned(),
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        self.reader
            .counters
            .snapshot(&self.config, self.file_readers.len(), files)
    }
    pub fn flush(&self) -> Result<()> {
        let _entered = self.runtime.enter();
        // Retained streams own repository holds. Release them before the
        // repository barrier, while the runtime is still alive.
        self.file_readers.clear(&self.runtime);
        self.runtime.block_on(self.reader.repository.flush())?;
        Ok(())
    }
}

pub fn import(source: &Path, destination: &Path, name: &str, descriptor: &str) -> Result<Snapshot> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let repository = casita::Repository::local(destination).await?;
        let key = repository
            .import(
                casita::import::FilesystemImport::new(source, casita::RootName::try_from(name)?)
                    .reread(true),
            )
            .await?;
        let digest = casita::DirectoryId::new(
            key.native_digest()
                .ok_or_else(|| anyhow!("non-native import"))?,
        );
        let directory = repository
            .directory(&digest)
            .await?
            .ok_or_else(|| anyhow!("missing imported root"))?;
        let snapshot = Snapshot {
            digest: digest.to_string(),
            size: directory.size(),
        };
        repository.flush().await?;
        let path = destination.join(descriptor);
        let temporary = path.with_extension("json.new");
        std::fs::write(&temporary, serde_json::to_vec(&snapshot)?)?;
        std::fs::rename(temporary, path)?;
        Ok(snapshot)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::Filesystem;

    #[test]
    fn directory_cache_controls_and_concurrent_enumeration() -> Result<()> {
        let base = tempfile::tempdir()?;
        let source = base.path().join("source");
        let repository = base.path().join("repository");
        std::fs::create_dir(&source)?;
        for name in ["z", "a"] {
            std::fs::write(source.join(name), name)?;
        }
        import(&source, &repository, "fixture", "snapshot.json")?;
        for cache_directories in [false, true] {
            for cache_enumeration in [false, true] {
                let backend = Backend::open_at(
                    &repository,
                    None,
                    BackendConfig {
                        cache_directories,
                        cache_enumeration,
                        ..BackendConfig::default()
                    },
                )?;
                // A miss loads metadata without allocating child inodes.
                assert!(backend.lookup(4, b"missing")?.is_none());
                assert!(!backend
                    .nodes
                    .lock()
                    .unwrap()
                    .children
                    .contains_key(&(4, b"a".to_vec())));
                let first = Filesystem::directory(&backend, 4)?;
                let second = Filesystem::directory(&backend, 4)?;
                assert_eq!(
                    Arc::ptr_eq(&first.0, &second.0),
                    cache_directories && cache_enumeration
                );
                assert_eq!(
                    backend.reader.counters.directories.load(Ordering::Relaxed),
                    if cache_directories { 1 } else { 3 }
                );
                assert_eq!(
                    first
                        .0
                        .iter()
                        .map(|entry| entry.name.as_slice())
                        .collect::<Vec<_>>(),
                    vec![b"a".as_slice(), b"z"]
                );
                for entry in first.0.iter() {
                    assert_eq!(backend.lookup(4, &entry.name)?.unwrap().id, entry.id);
                }
                backend.flush()?;
            }
        }
        let backend = Backend::open(&repository)?;
        assert!(backend.lookup(4, b"missing")?.is_none());
        let barrier = std::sync::Barrier::new(8);
        let snapshots = std::thread::scope(|scope| {
            let workers = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        Filesystem::directory(&backend, 4).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        for snapshot in &snapshots {
            assert!(Arc::ptr_eq(&snapshot.0, &snapshots[0].0));
            for entry in snapshot.0.iter() {
                assert_eq!(backend.lookup(4, &entry.name)?.unwrap().id, entry.id);
            }
        }
        backend.flush()?;
        Ok(())
    }

    #[test]
    fn cached_partial_reads_across_large_files_make_progress() -> Result<()> {
        const FILES: usize = 8;
        const SIZE: usize = 12 * 1024 * 1024;
        const PREFIX: usize = 2 * 1024 * 1024;
        let base = tempfile::tempdir()?;
        let source = base.path().join("source");
        let repository = base.path().join("repository");
        std::fs::create_dir(&source)?;
        // Distinct reproducible, poorly compressible files exceed the shared
        // 64 MiB prefetch budget well before the 32-reader eviction threshold.
        let mut state = 0x1234_5678_9abc_def0u64;
        for index in 0..FILES {
            let mut bytes = vec![0; SIZE];
            for word in bytes.chunks_mut(8) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                word.copy_from_slice(&state.to_le_bytes());
            }
            std::fs::write(source.join(format!("file-{index}")), bytes)?;
        }
        import(&source, &repository, "fixture", "snapshot.json")?;
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            // The worker owns the fixture and runtime, including on failure.
            let _base = base;
            let result = (|| -> Result<()> {
                let backend = Backend::open(&repository)?;
                let mut entries = Vec::new();
                for index in 0..FILES {
                    let entry = backend
                        .lookup(4, format!("file-{index}").as_bytes())?
                        .unwrap();
                    let expected = std::fs::read(source.join(format!("file-{index}")))?;
                    assert_eq!(
                        backend.read(entry.id, 0, PREFIX as u32)?,
                        expected[..PREFIX]
                    );
                    entries.push((entry, expected));
                }
                // Resume at the parked position, then seek backward. Neither
                // operation may reopen the blob or return stale prefetched data.
                for (entry, expected) in entries.iter().rev() {
                    assert_eq!(
                        backend.read(entry.id, PREFIX as u64, 4096)?,
                        expected[PREFIX..PREFIX + 4096]
                    );
                    assert_eq!(backend.read(entry.id, 123, 4096)?, expected[123..4219]);
                }
                assert_eq!(
                    backend.reader.counters.opens.load(Ordering::Relaxed),
                    FILES as u64
                );
                assert_eq!(
                    backend
                        .reader
                        .counters
                        .reader_evictions
                        .load(Ordering::Relaxed),
                    0
                );
                assert_eq!(backend.file_readers.len(), FILES);
                backend.flush()?;
                Ok(())
            })();
            let _ = send.send(result);
        });
        receive
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("cached partial reads stalled on the shared prefetch budget")?;
        worker.join().unwrap();
        Ok(())
    }

    #[test]
    fn real_snapshot_reads_nested_files_publishes_and_releases() -> Result<()> {
        let base = std::env::temp_dir().join(format!(
            "casita-native-repository-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(base.join("source/nested"))?;
        std::fs::write(base.join("source/nested/data"), b"repository bytes")?;
        for index in 0..=READER_CACHE_CAPACITY {
            std::fs::write(
                base.join(format!("source/reader-{index}")),
                [index as u8; 64],
            )?;
        }
        std::fs::write(
            base.join("source/nested/._present"),
            b"real dot-underscore file",
        )?;
        let snapshot = import(
            &base.join("source"),
            &base.join("repository"),
            "fixture",
            "snapshot.json",
        )?;
        std::fs::write(base.join("repository/trace-read-ranges"), b"")?;
        let backend = Backend::open(&base.join("repository"))?;
        assert!(backend.config.volume.store_timestamps);
        let independent = Backend::open(&base.join("repository"))?;
        std::fs::write(base.join("repository/reader-cache-capacity"), b"16")?;
        let limited = Backend::open(&base.join("repository"))?;
        assert_eq!(limited.stats()["native_reader_cache"]["capacity"], 16);
        for index in 0..=READER_CACHE_CAPACITY {
            let entry = limited
                .lookup(4, format!("reader-{index}").as_bytes())?
                .unwrap();
            assert_eq!(limited.read(entry.id, 3, 5)?, vec![index as u8; 5]);
        }
        assert_eq!(limited.stats()["native_reader_cache"]["resident"], 16);
        limited.flush()?;
        assert_eq!(limited.stats()["native_reader_cache"]["resident"], 0);
        drop(limited);
        std::fs::write(base.join("repository/reader-cache-capacity"), b"0")?;
        assert!(Backend::open(&base.join("repository")).is_err());
        std::fs::remove_file(base.join("repository/reader-cache-capacity"))?;
        let nested = backend.lookup(4, b"nested")?.unwrap();
        let file = backend.lookup(nested.id, b"data")?.unwrap();
        assert_eq!(backend.read(file.id, 2, 5)?, b"posit");
        assert_eq!(backend.read(file.id, 999, 5)?, b"");
        let opened = backend.reader.counters.opens.load(Ordering::Relaxed);
        assert_eq!(backend.read(file.id, 0, 10)?, b"repository");
        let trace = backend.stats()["native_read_trace"].clone();
        assert_eq!(trace["enabled"], true);
        assert_eq!(trace["dropped"], 0);
        assert_eq!(trace["ranges"][format!("{}:2:5", file.id)][0], 1);
        assert_eq!(trace["ranges"][format!("{}:2:5", file.id)][1], 5);
        assert_eq!(trace["ranges"][format!("{}:999:5", file.id)][1], 0);
        assert_eq!(
            backend.reader.counters.opens.load(Ordering::Relaxed),
            opened
        );
        std::thread::scope(|scope| {
            for offset in [0, 2, 5, 10] {
                let backend = &backend;
                let id = file.id;
                scope.spawn(move || {
                    for _ in 0..16 {
                        assert_eq!(
                            backend.read(id, offset, 4).unwrap(),
                            b"repository bytes"[offset as usize..offset as usize + 4]
                        );
                    }
                });
            }
        });
        for index in 0..=READER_CACHE_CAPACITY {
            let entry = backend
                .lookup(4, format!("reader-{index}").as_bytes())?
                .unwrap();
            assert_eq!(backend.read(entry.id, 3, 5)?, vec![index as u8; 5]);
        }
        assert_eq!(backend.file_readers.len(), READER_CACHE_CAPACITY);
        let opened = backend.reader.counters.opens.load(Ordering::Relaxed);
        assert_eq!(backend.read(file.id, 2, 5)?, b"posit");
        assert_eq!(
            backend.reader.counters.opens.load(Ordering::Relaxed),
            opened + 1
        );
        assert_eq!(backend.lookup(nested.id, b"data")?.unwrap().id, file.id);
        let directory_calls = backend.reader.counters.directories.load(Ordering::Relaxed);
        // macOS probes these names during exec. A missing-name cache must not
        // hide a real ._ file, reread storage, or grow with arbitrary misses.
        for name in [b"._data".as_slice(), b"another-missing-name", b"._data"] {
            assert!(backend.lookup(nested.id, name)?.is_none());
        }
        let sidecar = backend.lookup(nested.id, b"._present")?.unwrap();
        assert_eq!(
            backend.read(sidecar.id, 0, 99)?,
            b"real dot-underscore file"
        );
        assert_eq!(backend.entries(nested.id)?.len(), 2);
        let first = Filesystem::directory(&backend, nested.id)?;
        let second = Filesystem::directory(&backend, nested.id)?;
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(
            first
                .0
                .iter()
                .map(|e| e.name.as_slice())
                .collect::<Vec<_>>(),
            vec![b"._present".as_slice(), b"data"]
        );
        assert_eq!(
            backend.reader.counters.directories.load(Ordering::Relaxed),
            directory_calls
        );
        assert_eq!(
            backend.nodes.lock().unwrap().directories[&nested.id]
                .iter()
                .count(),
            2
        );
        assert!(backend.lookup(3, b"later")?.is_none());
        let namespace_before = Filesystem::directory(&backend, 3)?;
        std::fs::write(
            base.join("repository/stage-later.json"),
            serde_json::to_vec(&snapshot)?,
        )?;
        assert!(backend.publish(b"../snapshot").is_err());
        let published = backend.publish(b"later")?;
        assert!(!namespace_before
            .0
            .iter()
            .any(|entry| entry.name == b"later"));
        assert!(Filesystem::directory(&backend, 3)?
            .0
            .iter()
            .any(|entry| entry.name == b"later"));
        assert_eq!(
            Filesystem::directory(&backend, 3)?.0.len(),
            namespace_before.0.len() + 1
        );
        assert!(!backend.nodes.lock().unwrap().directories.contains_key(&3));
        assert_eq!(published.node, backend.entry(4)?.node);
        assert!(backend.publish(b"later").is_err());
        assert!(independent.lookup(3, b"later")?.is_none());
        backend.flush()?;
        drop(backend);
        assert!(independent.lookup(4, b"nested")?.is_some());
        independent.flush()?;
        drop(independent);
        std::fs::write(base.join("repository/disable-enumeration-cache"), b"")?;
        std::fs::write(base.join("repository/zero-timestamps"), b"")?;
        let entries_uncached = Backend::open(&base.join("repository"))?;
        assert!(!entries_uncached.config.volume.store_timestamps);
        assert!(!Arc::ptr_eq(
            &Filesystem::directory(&entries_uncached, 4)?.0,
            &Filesystem::directory(&entries_uncached, 4)?.0
        ));
        // Disabling entry reuse must not disable the older repository metadata cache.
        assert_eq!(
            entries_uncached
                .reader
                .counters
                .directories
                .load(Ordering::Relaxed),
            1
        );
        entries_uncached.flush()?;
        drop(entries_uncached);
        // Keep an executable regression control using the same binary/backend.
        std::fs::write(base.join("repository/disable-directory-cache"), b"")?;
        std::fs::write(base.join("repository/disable-reader-cache"), b"")?;
        let uncached = Backend::open(&base.join("repository"))?;
        assert!(!Arc::ptr_eq(
            &Filesystem::directory(&uncached, 4)?.0,
            &Filesystem::directory(&uncached, 4)?.0
        ));
        let before = uncached.reader.counters.directories.load(Ordering::Relaxed);
        assert!(uncached.lookup(4, b"absent")?.is_none());
        assert!(uncached.lookup(4, b"absent")?.is_none());
        assert_eq!(
            uncached.reader.counters.directories.load(Ordering::Relaxed),
            before + 2
        );
        let nested = uncached.lookup(4, b"nested")?.unwrap();
        let file = uncached.lookup(nested.id, b"data")?.unwrap();
        let opens = uncached.reader.counters.opens.load(Ordering::Relaxed);
        for offset in [0, 2] {
            assert_eq!(
                uncached.read(file.id, offset, 4)?,
                b"repository bytes"[offset as usize..offset as usize + 4]
            );
        }
        assert_eq!(
            uncached.reader.counters.opens.load(Ordering::Relaxed),
            opens + 2
        );
        assert_eq!(uncached.file_readers.len(), 0);
        uncached.flush()?;
        drop(uncached);
        std::fs::remove_dir_all(base)?;
        Ok(())
    }
}
