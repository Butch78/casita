//! Measure what a mount costs, without a build in the way.
//!
//! Imports a host directory into a scratch casita store, mounts it, and walks
//! it: first straight off the host filesystem for a baseline, then twice
//! through the mount. The second round through the mount answers whether the
//! kernel's caches absorb the cost or whether every pass pays again, and the
//! per-operation counters attribute that cost to round trips rather than
//! bytes.
//!
//! ```console
//! cargo run --release --example bench -- /nix/store/…-glibc-2.38-27-dev
//! ```

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This benchmark measures the Linux FUSE transport. Use the FSKit tests on macOS.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;

    use casita_fs::fuse::{FuseMount, Op, StoreFs};
    use casita_fs::{ContentReader, FilesystemView};

    pub(super) fn run() -> anyhow::Result<()> {
        let source: PathBuf = std::env::args()
            .nth(1)
            .expect("usage: bench <host-path> [threads]")
            .into();
        let threads: usize = std::env::args().nth(2).map_or(4, |t| t.parse().unwrap());
        let name = source
            .file_name()
            .expect("a store path basename")
            .to_string_lossy()
            .into_owned();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        // Baseline: the same walk against the real filesystem, twice, so the
        // comparison is against warm page cache rather than a cold disk.
        for round in ["host-cold", "host-warm"] {
            let (walk_ms, read_ms, files, bytes) = timed_walk(&source)?;
            println!(
            "{round:<10} walk {walk_ms:>8.1}ms  read {read_ms:>8.1}ms  files={files} bytes={bytes}"
        );
        }

        let scratch = tempfile::tempdir()?;
        let repository = runtime.block_on(casita::Repository::local(scratch.path()))?;
        let node = runtime.block_on(async {
            let key = repository
                .import(
                    casita::import::FilesystemImport::new(
                        &source,
                        casita::RootName::try_from("benchmark")?,
                    )
                    .reread(true),
                )
                .await?;
            let digest = casita::DirectoryId::new(
                key.native_digest()
                    .ok_or_else(|| anyhow::anyhow!("non-native directory"))?,
            );
            let directory = repository
                .directory(&digest)
                .await?
                .ok_or_else(|| anyhow::anyhow!("missing imported directory"))?;
            Ok::<_, anyhow::Error>(casita::Node::Directory {
                digest,
                size: directory.size(),
            })
        })?;

        let view = FilesystemView::new(
            Arc::new(repository),
            [(name.as_bytes().to_vec(), node)].into_iter().collect(),
        );
        // How much of an operation is the hop into tokio and back? Every
        // FUSE handler pays this once, on a thread that is not a runtime
        // worker, before it touches the store at all.
        let start = Instant::now();
        for _ in 0..10_000 {
            runtime.block_on(async {});
        }
        println!(
            "block_on:  {:>8.1}us per empty future",
            start.elapsed().as_secs_f64() * 1e6 / 10_000.0
        );

        let mountpoint = std::env::temp_dir().join(format!("storefs-bench-{}", std::process::id()));
        fs::create_dir_all(&mountpoint)?;
        let mount = FuseMount::new(
            StoreFs::new(view, runtime.handle().clone()),
            &mountpoint,
            threads,
        )?;

        let tree = mountpoint.join(&name);
        for round in ["mount-cold", "mount-warm"] {
            let before = mount.stats();
            let (walk_ms, read_ms, files, bytes) = timed_walk(&tree)?;
            let after = mount.stats();
            println!(
            "{round:<10} walk {walk_ms:>8.1}ms  read {read_ms:>8.1}ms  files={files} bytes={bytes}"
        );
            println!(
                "           ops: lookup={} getattr={} readdir={} open={} read={}",
                after.calls(Op::Lookup) - before.calls(Op::Lookup),
                after.calls(Op::Getattr) - before.calls(Op::Getattr),
                after.calls(Op::Readdir) - before.calls(Op::Readdir),
                after.calls(Op::Open) - before.calls(Op::Open),
                after.calls(Op::Read) - before.calls(Op::Read),
            );
        }
        // Execution is the shape configure has: a short-lived process,
        // started thousands of times, reading its own image through the
        // mount each time.
        if let Some(program) = std::env::args().nth(3) {
            let binary = tree.join(&program);
            for round in ["exec-cold", "exec-warm"] {
                let before = mount.stats();
                let start = Instant::now();
                for _ in 0..200 {
                    let status = std::process::Command::new(&binary).status()?;
                    anyhow::ensure!(status.success(), "{} failed", binary.display());
                }
                let after = mount.stats();
                println!(
                "{round:<10} 200 execs in {:>8.1}ms  ops: lookup={} getattr={} open={} read={} bytes={}",
                start.elapsed().as_secs_f64() * 1e3,
                after.calls(Op::Lookup) - before.calls(Op::Lookup),
                after.calls(Op::Getattr) - before.calls(Op::Getattr),
                after.calls(Op::Open) - before.calls(Op::Open),
                after.calls(Op::Read) - before.calls(Op::Read),
                after.bytes_read() - before.bytes_read(),
            );
            }
        }

        println!("totals:    {}", mount.stats());

        mount.unmount()?;
        let _ = fs::remove_dir(&mountpoint);
        Ok(())
    }

    /// Walk once for metadata, then again reading every byte.
    fn timed_walk(path: &Path) -> anyhow::Result<(f64, f64, u64, u64)> {
        let start = Instant::now();
        let (files, bytes) = walk(path, false)?;
        let walk_ms = start.elapsed().as_secs_f64() * 1e3;
        let start = Instant::now();
        let (_, read) = walk(path, true)?;
        Ok((
            walk_ms,
            start.elapsed().as_secs_f64() * 1e3,
            files,
            read.max(bytes),
        ))
    }

    fn walk(path: &Path, contents: bool) -> anyhow::Result<(u64, u64)> {
        let mut files = 0;
        let mut bytes = 0;
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let meta = entry.metadata()?;
                if meta.is_dir() {
                    stack.push(entry.path());
                } else if meta.is_file() {
                    files += 1;
                    bytes += if contents {
                        fs::read(entry.path())?.len() as u64
                    } else {
                        meta.len()
                    };
                }
            }
        }
        Ok((files, bytes))
    }
}
