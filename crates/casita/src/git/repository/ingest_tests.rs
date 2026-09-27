use super::*;
use async_trait::async_trait;
use futures::FutureExt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

struct Source(tempfile::TempDir);

impl Source {
    fn new(files: usize, packed: bool) -> Self {
        let source = Self(tempfile::tempdir().unwrap());
        source.git(&["init", "-q", "-b", "main", "--object-format=sha1"]);
        source.commit(files, 0, packed);
        source
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(self.0.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.0.path().join("no-config"))
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn commit(&self, files: usize, version: usize, packed: bool) {
        for index in (0..files).step_by(if version == 0 { 1 } else { 4 }) {
            let size = if index.is_multiple_of(16) {
                65536
            } else {
                1024
            };
            let mut bytes = vec![index as u8; size];
            bytes[..16].copy_from_slice(format!("file{index:06}{version:06}").as_bytes());
            std::fs::write(self.0.path().join(format!("{index:06}")), bytes).unwrap();
        }
        self.git(&["add", "."]);
        self.git(&["commit", "-q", "-m", &format!("version {version}")]);
        if packed {
            self.git(&["repack", "-adf", "--window=16"]);
        }
    }

    fn inventory(&self) -> BTreeSet<ObjectKey> {
        use std::io::Write;
        let ids = self.git(&["rev-list", "--objects", "--all"]);
        use std::io::{Seek, SeekFrom};
        let mut input = tempfile::tempfile().unwrap();
        for line in ids.lines() {
            writeln!(input, "{}", line.split_whitespace().next().unwrap()).unwrap();
        }
        input.seek(SeekFrom::Start(0)).unwrap();
        let output = self
            .command()
            .args(["cat-file", "--batch-check"])
            .stdin(Stdio::from(input))
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                let kind = match fields[1] {
                    "blob" => crate::GitObjectKind::Blob,
                    "tree" => crate::GitObjectKind::Tree,
                    "commit" => crate::GitObjectKind::Commit,
                    "tag" => crate::GitObjectKind::Tag,
                    _ => panic!("unexpected object kind"),
                };
                crate::git::git_object_key(
                    GitObjectFormat::Sha1,
                    kind,
                    data_encoding::HEXLOWER
                        .decode(fields[0].as_bytes())
                        .unwrap(),
                )
                .unwrap()
            })
            .collect()
    }
}

#[derive(Default)]
struct Activity {
    active: AtomicUsize,
    bytes: AtomicU64,
    peak: AtomicUsize,
    peak_bytes: AtomicU64,
    mode: AtomicUsize, // 0: normal, 1: first blob waits / others fail, 2: all blobs wait
    started: tokio::sync::Notify,
}

struct ActivePut<'a> {
    activity: &'a Activity,
    bytes: u64,
}
impl Drop for ActivePut<'_> {
    fn drop(&mut self) {
        self.activity.bytes.fetch_sub(self.bytes, Ordering::SeqCst);
        self.activity.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct MeasuredStore {
    inner: crate::MemoryBlobStore,
    activity: Arc<Activity>,
    concurrency: usize,
    budget: u64,
    delay_ms: u64,
}

#[async_trait]
impl crate::blob::CatalogPublication for MeasuredStore {
    async fn refresh_discovery(&self) -> Result<(), crate::error::Error> {
        self.inner.publication().refresh_discovery().await
    }
    async fn flush(&self) -> Result<(), crate::error::Error> {
        assert_eq!(
            self.activity.active.load(Ordering::SeqCst),
            0,
            "checkpoint must finish active writes before flushing storage"
        );
        self.inner.publication().flush().await
    }
    async fn synchronize_state_catalog(
        &self,
        catalog: Option<&[u8]>,
    ) -> Result<(), crate::error::Error> {
        self.inner
            .publication()
            .synchronize_state_catalog(catalog)
            .await
    }
    fn enable_state_catalog(&self) {
        self.inner.publication().enable_state_catalog();
    }
    async fn prepare_state_commit(
        &self,
    ) -> Result<crate::blob::PreparedCatalog, crate::error::Error> {
        self.inner.publication().prepare_state_commit().await
    }
    fn take_catalog_maintenance(&self) -> Option<crate::blob::CatalogMaintenance> {
        self.inner.publication().take_catalog_maintenance()
    }
}

#[async_trait]
impl BlobStore for MeasuredStore {
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }
    fn begin_pinned_batch(
        &self,
        pin: crate::metadata::DataPinLease,
    ) -> Result<crate::blob::BlobBatchGuard, crate::error::Error> {
        self.inner.begin_pinned_batch(pin)
    }
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        crate::blob::PayloadPublication::Cataloged(self)
    }
    async fn has(&self, id: &crate::BlobId) -> Result<bool, crate::error::Error> {
        self.inner.has(id).await
    }
    async fn open_read(
        &self,
        id: &crate::BlobId,
    ) -> Result<Option<Box<dyn crate::blob::BlobReader>>, crate::error::Error> {
        self.inner.open_read(id).await
    }
    async fn open_write(&self) -> Box<dyn crate::blob::BlobWriter> {
        self.inner.open_write().await
    }
    async fn put_slice(&self, data: &[u8]) -> Result<crate::BlobId, crate::error::Error> {
        let bytes = data.len() as u64;
        let active = self.activity.active.fetch_add(1, Ordering::SeqCst) + 1;
        let total = self.activity.bytes.fetch_add(bytes, Ordering::SeqCst) + bytes;
        let _guard = ActivePut {
            activity: &self.activity,
            bytes,
        };
        self.activity.peak.fetch_max(active, Ordering::SeqCst);
        self.activity.peak_bytes.fetch_max(total, Ordering::SeqCst);
        assert!(active <= self.concurrency);
        assert!(
            total <= self.budget || (active == 1 && bytes > self.budget),
            "oversized objects must be exclusive"
        );
        if data.starts_with(b"file") {
            match self.activity.mode.load(Ordering::SeqCst) {
                1 if !data.starts_with(b"file000000") => {
                    return Err(crate::error::Error::from("injected staging failure"));
                }
                1 | 2 => {
                    self.activity.started.notify_one();
                    std::future::pending::<()>().await;
                }
                _ => {}
            }
        }
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        self.inner.put_slice(data).await
    }
}

fn repository(
    concurrency: usize,
    budget: u64,
    delay_ms: u64,
) -> (
    Repository<MeasuredStore, crate::MemoryMetadataStore>,
    Arc<Activity>,
) {
    let activity = Arc::new(Activity::default());
    let store = MeasuredStore {
        inner: crate::MemoryBlobStore::new(),
        activity: activity.clone(),
        concurrency,
        budget,
        delay_ms,
    };
    let limits = crate::FormatLimits {
        max_batch_objects: 7,
        ..Default::default()
    };
    (
        Repository::with_formats(
            store,
            crate::MemoryMetadataStore::new().unwrap(),
            crate::FormatRegistry::builtin(),
            limits,
        ),
        activity,
    )
}

fn options(concurrency: usize, budget: u64) -> NativeGitImportOptions {
    NativeGitImportOptions {
        view_name: "bench".into(),
        max_cached_pack_bytes: 0,
        concurrency: NonZeroUsize::new(concurrency).unwrap(),
        max_buffered_bytes: NonZeroU64::new(budget).unwrap(),
        ..Default::default()
    }
}

async fn validate(
    repository: &Repository<MeasuredStore, crate::MemoryMetadataStore>,
    source: &Source,
    outcome: &NativeGitImportOutcome,
) {
    let (key, view) = read_git_view(repository, "bench").await.unwrap().unwrap();
    assert_eq!(key, outcome.view);
    assert_eq!(*view.objects(), source.inventory());
    assert_eq!(view.objects().len(), outcome.objects);
    assert!(matches!(
        repository.verify_closure(&key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
}

#[tokio::test]
async fn object_and_byte_limits_preserve_initial_incremental_and_unchanged_views() {
    let mut expected = BTreeMap::new();
    for packed in [false, true] {
        for (concurrency, budget) in [
            (1, 65536),
            (16, 65535),
            (16, 65536),
            (16, 65537),
            (16, 64 * 1024 * 1024),
        ] {
            let source = Source::new(17, packed);
            let (repository, activity) = repository(concurrency, budget, 1);
            let options = options(concurrency, budget);
            for version in 0..2 {
                if version > 0 {
                    source.commit(17, version, packed);
                }
                let result = repository
                    .import_native_git_view(source.0.path(), &options)
                    .await
                    .unwrap();
                validate(&repository, &source, &result).await;
                assert_eq!(
                    &result.view,
                    expected
                        .entry(version)
                        .or_insert_with(|| result.view.clone())
                );
                let unchanged = repository
                    .import_native_git_view(source.0.path(), &options)
                    .await
                    .unwrap();
                assert_eq!(unchanged, result);
            }
            assert_eq!(activity.active.load(Ordering::SeqCst), 0);
            assert_eq!(activity.bytes.load(Ordering::SeqCst), 0);
            if concurrency > 1 {
                assert!(activity.peak.load(Ordering::SeqCst) > 1);
            }
        }
    }
}

#[tokio::test]
async fn failed_and_cancelled_imports_drop_work_and_preserve_the_previous_root() {
    let source = Source::new(17, false);
    let (repository, activity) = repository(16, 64 * 1024 * 1024, 0);
    let options = options(16, 64 * 1024 * 1024);
    let original = repository
        .import_native_git_view(source.0.path(), &options)
        .await
        .unwrap();
    source.commit(17, 1, false);
    for mode in [1, 2] {
        while activity.started.notified().now_or_never().is_some() {}
        activity.mode.store(mode, Ordering::SeqCst);
        let import = repository.import_native_git_view(source.0.path(), &options);
        {
            tokio::pin!(import);
            if mode == 1 {
                let error = tokio::time::timeout(Duration::from_secs(5), &mut import)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(error.to_string().contains("injected staging failure"));
            } else {
                tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::select! {
                        _ = &mut import => panic!("pending upload completed"),
                        _ = activity.started.notified() => {},
                    }
                })
                .await
                .unwrap();
                assert!(activity.active.load(Ordering::SeqCst) > 0);
            }
        }
        // Drop the future itself, not just its pinned reference.
        assert_eq!(activity.active.load(Ordering::SeqCst), 0);
        assert_eq!(activity.bytes.load(Ordering::SeqCst), 0);
        assert_eq!(
            read_git_view(&repository, "bench")
                .await
                .unwrap()
                .unwrap()
                .0,
            original.view
        );
    }
}

/// Real Git import with controlled payload-store latency; source setup and
/// independent Git inventory/closure audits are outside the timed region.
#[tokio::test]
#[ignore = "run through benchmark run git-ingest-scheduling"]
async fn benchmark_git_ingest_scheduling() {
    let number = |name| std::env::var(name).unwrap().parse::<usize>().unwrap();
    let files = number("CASITA_GIT_FILES");
    let concurrency = number("CASITA_GIT_CONCURRENCY");
    let budget = number("CASITA_GIT_BUFFERED_BYTES") as u64;
    let delay_ms = number("CASITA_GIT_DELAY_MS") as u64;
    let packed = number("CASITA_GIT_PACKED") != 0;
    let source = Source::new(files, packed);
    let (repository, activity) = repository(concurrency, budget, delay_ms);
    let options = options(concurrency, budget);
    for version in 0..2 {
        if version > 0 {
            source.commit(files, version, packed);
        }
        activity.peak.store(0, Ordering::SeqCst);
        activity.peak_bytes.store(0, Ordering::SeqCst);
        let started = std::time::Instant::now();
        let result = repository
            .import_native_git_view(source.0.path(), &options)
            .await
            .unwrap();
        let wall_nanos = started.elapsed().as_nanos() as u64;
        validate(&repository, &source, &result).await;
        assert_eq!(activity.active.load(Ordering::SeqCst), 0);
        assert_eq!(activity.bytes.load(Ordering::SeqCst), 0);
        println!(
            "git_ingest_sample {}",
            serde_json::json!({
                "files": files, "concurrency": concurrency, "max_buffered_bytes": budget, "delay_ms": delay_ms, "packed": packed,
                "operation": if version == 0 { "initial-import" } else { "incremental-import" },
                "wall_nanos": wall_nanos, "peak_active": activity.peak.load(Ordering::SeqCst),
                "peak_bytes": activity.peak_bytes.load(Ordering::SeqCst), "root": result.view.to_string(),
                "correctness": "exact independent Git inventory and complete closure; bounded active puts and bytes; oversized objects exclusive"
            })
        );
    }
}
