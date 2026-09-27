//! Disposable mount process controlled by benchmarks.suites.filesystem_transports.
//! Stdin/stdout are private inherited pipes, not a listening control endpoint.
use anyhow::{anyhow, ensure, Result};
use casita_fs::{ContentReader, ContentStream, FilesystemView};
use serde_json::json;
use std::io::{self, BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

struct Reader {
    repository: casita::Repository,
    directories: AtomicU64,
    opens: AtomicU64,
    open_nanos: AtomicU64,
}

#[async_trait::async_trait]
impl ContentReader for Reader {
    async fn directory(&self, key: &casita::DirectoryId) -> io::Result<Option<casita::Directory>> {
        self.directories.fetch_add(1, Ordering::Relaxed);
        self.repository.directory(key).await
    }
    async fn open_blob(&self, key: &casita::BlobId) -> io::Result<Option<Box<dyn ContentStream>>> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let start = Instant::now();
        let result = self.repository.open_blob(key).await;
        self.open_nanos
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        result
    }
    async fn checkout_directory(&self, key: &casita::DirectoryId, path: &Path) -> io::Result<()> {
        self.repository.checkout_directory(key, path).await
    }
}

fn emit(value: serde_json::Value) -> Result<()> {
    println!("{value}");
    io::stdout().flush()?;
    Ok(())
}

fn usage() -> serde_json::Value {
    // SAFETY: getrusage initializes the supplied structure on success.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return json!({"error": io::Error::last_os_error().to_string()});
    }
    let usage = unsafe { usage.assume_init() };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
    json!({
        "user_seconds": seconds(usage.ru_utime), "system_seconds": seconds(usage.ru_stime),
        "max_rss_bytes": usage.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 },
    })
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: transport_server SOURCE WORK SERVER_THREADS"
    );
    let source = PathBuf::from(&args[0]).canonicalize()?;
    let work = PathBuf::from(&args[1]).canonicalize()?;
    let threads: usize = args[2]
        .to_str()
        .ok_or_else(|| anyhow!("invalid threads"))?
        .parse()?;
    ensure!(threads > 0, "threads must be positive");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    let repository = runtime.block_on(casita::Repository::local(work.join("repository")))?;
    let import_started = Instant::now();
    let node = runtime.block_on(async {
        let key = repository
            .import(
                casita::import::FilesystemImport::new(
                    &source,
                    casita::RootName::try_from("transport-fixture")?,
                )
                .reread(true),
            )
            .await?;
        let digest =
            casita::DirectoryId::new(key.native_digest().ok_or_else(|| anyhow!("not native"))?);
        let directory = repository
            .directory(&digest)
            .await?
            .ok_or_else(|| anyhow!("missing directory"))?;
        Ok::<_, anyhow::Error>(casita::Node::Directory {
            digest,
            size: directory.size(),
        })
    })?;
    let import_seconds = import_started.elapsed().as_secs_f64();
    let reader = Arc::new(Reader {
        repository,
        directories: AtomicU64::new(0),
        opens: AtomicU64::new(0),
        open_nanos: AtomicU64::new(0),
    });
    let make_view = |name: &[u8]| {
        FilesystemView::new(
            reader.clone(),
            [(name.to_vec(), node.clone())].into_iter().collect(),
        )
    };
    let mount_started = Instant::now();
    #[cfg(target_os = "linux")]
    let (mount, tree) = {
        let path = work.join("mount");
        std::fs::create_dir(&path)?;
        let mount = casita_fs::fuse::FuseMount::new(
            casita_fs::fuse::StoreFs::new(make_view(b"fixture"), runtime.handle().clone()),
            &path,
            threads,
        )?;
        (mount, path.join("fixture"))
    };
    #[cfg(target_os = "macos")]
    let (mut mount, tree) = {
        // Native FSKit controls callback concurrency.
        ensure!(threads == 1, "native FSKit controls server concurrency");
        runtime.block_on(reader.repository.flush())?;
        let mut mount = casita_fs::darwin::PersistentMount::new(&work.join("repository"), &work)?;
        let tree = mount.publish_root(b"fixture", node.clone())?;
        (mount, tree)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    compile_error!("transport_server requires Linux or macOS");
    let mount_seconds = mount_started.elapsed().as_secs_f64();
    #[cfg(target_os = "linux")]
    let mountpoint = mount.mountpoint();
    #[cfg(target_os = "macos")]
    let mountpoint = mount.path();
    emit(
        json!({"event": "ready", "tree": tree, "import_seconds": import_seconds,
        "mountpoint": mountpoint,
        "mount_seconds": mount_seconds, "transport": if cfg!(target_os = "macos") { "native-fskit" } else { "linux-fuse" },
        "usage": usage(), "server_threads": threads}),
    )?;
    #[cfg(target_os = "macos")]
    let mut stats_sequence = 0u32;
    for line in io::stdin().lock().lines() {
        match line?.as_str() {
            "stats" => {
                #[cfg(target_os = "linux")]
                let fuse = {
                    use casita_fs::fuse::Op;
                    let stats = mount.stats();
                    json!({"lookup": stats.calls(Op::Lookup), "getattr": stats.calls(Op::Getattr),
                        "readdir": stats.calls(Op::Readdir), "open": stats.calls(Op::Open),
                        "read": stats.calls(Op::Read), "bytes_read": stats.bytes_read(),
                        "handler_summary": stats.to_string()})
                };
                #[cfg(target_os = "macos")]
                {
                    let mut stats: serde_json::Value = serde_json::from_slice(&std::fs::read(
                        mount
                            .path()
                            .join(format!("__casita_stats-{stats_sequence}")),
                    )?)?;
                    stats_sequence += 1;
                    stats["blob_open_nanos"] = stats["open_ns"].clone();
                    stats["usage"] = usage();
                    stats["fuse"] = serde_json::Value::Null;
                    emit(stats)?;
                }
                #[cfg(target_os = "linux")]
                emit(
                    json!({"directories": reader.directories.load(Ordering::Relaxed),
                    "blob_opens": reader.opens.load(Ordering::Relaxed),
                    "blob_open_nanos": reader.open_nanos.load(Ordering::Relaxed),
                    "usage": usage(), "fuse": fuse}),
                )?;
            }
            command @ ("direct-reads" | "direct-retained-reads") => {
                // Async reader destructors must be able to schedule pin cleanup.
                let _entered = runtime.enter();
                runtime.block_on(reader.repository.flush())?;
                eprintln!("{command}: preparing cases");
                // Run after mounted cases: this bypasses FUSE and its page cache.
                // Source reads and directory resolution are outside the timed loop.
                let view = make_view(b"fixture");
                let root = view.root(b"fixture").expect("fixture root");
                let entries = runtime.block_on(view.entries(&root))?;
                let mut cases = Vec::new();
                for entry in entries {
                    if entry.node.kind() != casita_fs::FilesystemNodeKind::Regular
                        || entry.name == b"echo"
                    {
                        continue;
                    }
                    let path = source.join(std::ffi::OsStr::from_bytes(&entry.name));
                    // Match the Python small/boundary case, excluding the bulk file.
                    if path.metadata()?.len() > 131_073 {
                        continue;
                    }
                    cases.push((entry, std::fs::read(path)?));
                }
                cases.sort_by(|a, b| a.0.name.cmp(&b.0.name));
                let admission = Instant::now();
                let retained = if command == "direct-retained-reads" {
                    Some(runtime.block_on(reader.repository.retained_reader())?)
                } else {
                    None
                };
                let admission_seconds = admission.elapsed().as_secs_f64();
                let mut passes = Vec::new();
                for pass in 0..2 {
                    eprintln!("{command}: pass {pass}, {} files", cases.len());
                    let before = usage();
                    let start = Instant::now();
                    let mut observations = Vec::new();
                    for (index, (entry, expected)) in cases.iter().enumerate() {
                        let opened = Instant::now();
                        let mut stream: Box<dyn ContentStream> = if let Some(retained) = &retained {
                            let casita_fs::ContentKey::Regular { digest, .. } =
                                entry.node.content_key()
                            else {
                                unreachable!("regular cases only")
                            };
                            Box::new(
                                runtime
                                    .block_on(retained.open(&casita::ObjectKey::blob(
                                        casita::BlobId::new(digest),
                                    )))?
                                    .ok_or_else(|| anyhow!("retained blob missing"))?,
                            )
                        } else {
                            runtime.block_on(view.open(&entry.node))?
                        };
                        let open_nanos = opened.elapsed().as_nanos() as u64;
                        let reading = Instant::now();
                        let mut bytes = Vec::new();
                        runtime.block_on(tokio::io::AsyncReadExt::read_to_end(
                            &mut stream,
                            &mut bytes,
                        ))?;
                        let read_nanos = reading.elapsed().as_nanos() as u64;
                        let dropping = Instant::now();
                        drop(stream);
                        let drop_nanos = dropping.elapsed().as_nanos() as u64;
                        ensure!(&bytes == expected, "direct repository bytes differ");
                        observations.push(json!({"name_bytes": entry.name, "bytes": bytes.len(),
                            "open_nanos": open_nanos, "read_nanos": read_nanos, "drop_nanos": drop_nanos}));
                        if (index + 1) % 256 == 0 {
                            eprintln!("{command}: pass {pass}, {} files complete", index + 1);
                        }
                    }
                    passes.push(json!({"pass": pass, "wall_seconds": start.elapsed().as_secs_f64(),
                        "usage_before": before, "usage_after": usage(), "observations": observations,
                        "correctness": "passed"}));
                    let cleanup = Instant::now();
                    runtime.block_on(reader.repository.flush())?;
                    passes.last_mut().unwrap()["cleanup_seconds"] =
                        json!(cleanup.elapsed().as_secs_f64());
                }
                let dropping = Instant::now();
                drop(retained);
                runtime.block_on(reader.repository.flush())?;
                eprintln!("{command}: complete");
                emit(json!({"event": command, "passes": passes,
                    "admission_seconds": admission_seconds, "session_drop_seconds": dropping.elapsed().as_secs_f64()}))?;
            }
            "publish" => {
                #[cfg(target_os = "macos")]
                {
                    let path = mount.publish_root(b"published", node.clone())?;
                    emit(json!({"path": path}))?;
                }
                #[cfg(target_os = "linux")]
                emit(json!({"unsupported": "this Linux fixture uses a fixed view"}))?;
            }
            "checkout" => {
                let casita::Node::Directory { digest, .. } = &node else {
                    unreachable!("directory fixture")
                };
                let destination = work.join("checkout");
                let started = Instant::now();
                runtime.block_on(reader.checkout_directory(digest, &destination))?;
                emit(json!({"event": "checkout", "path": destination,
                    "checkout_seconds": started.elapsed().as_secs_f64()}))?;
            }
            "stop" => break,
            _ => anyhow::bail!("unknown control command"),
        }
    }
    let unmount_started = Instant::now();
    #[cfg(target_os = "linux")]
    mount.unmount()?;
    #[cfg(target_os = "macos")]
    mount.close()?;
    drop(mount);
    runtime.block_on(reader.repository.flush())?;
    // The Python controller independently checks the mount table after this.
    emit(
        json!({"event": "stopped", "unmount_seconds": unmount_started.elapsed().as_secs_f64(), "usage": usage()}),
    )?;
    Ok(())
}
