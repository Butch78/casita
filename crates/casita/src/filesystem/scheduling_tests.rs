use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;

struct ActiveRead<'a>(&'a AtomicUsize);

impl<'a> ActiveRead<'a> {
    fn enter(active: &'a AtomicUsize, peak: &AtomicUsize) -> Self {
        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(count, Ordering::SeqCst);
        Self(active)
    }
}

impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn fixture(files: usize) -> (tempfile::TempDir, Vec<FilesystemEntry>) {
    let directory = tempfile::tempdir().unwrap();
    let mut expected = Vec::new();
    for index in 0..files {
        let path = PathBuf::from(format!("{index:06}"));
        let size = if index.is_multiple_of(16) {
            64 * 1024
        } else {
            1024
        };
        let data: Vec<_> = (0..size).map(|byte| ((byte + index) % 251) as u8).collect();
        std::fs::write(directory.path().join(&path), &data).unwrap();
        expected.push(FilesystemEntry::Regular {
            path,
            size: data.len() as u64,
            executable: false,
            digest: BlobId::new(blake3::hash(&data).into()),
        });
    }
    expected.push(FilesystemEntry::Directory {
        path: PathBuf::new(),
    });
    (directory, expected)
}

#[tokio::test]
async fn cached_files_directories_and_exclusions_keep_post_order_across_pages() {
    let directory = tempfile::tempdir().unwrap();
    let digest = BlobId::new(blake3::hash(b"x").into());
    let mut expected = Vec::new();
    for group in ["a", "z"] {
        std::fs::create_dir(directory.path().join(group)).unwrap();
        for index in 0..520 {
            let path = PathBuf::from(format!("{group}/{index:06}"));
            std::fs::write(directory.path().join(&path), b"x").unwrap();
            if path != std::path::Path::new("a/000005") {
                expected.push(FilesystemEntry::Regular {
                    path,
                    size: 1,
                    executable: false,
                    digest,
                });
            }
        }
        expected.push(FilesystemEntry::Directory {
            path: PathBuf::from(group),
        });
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a/000000", directory.path().join("z-link")).unwrap();
        expected.push(FilesystemEntry::Symlink {
            path: PathBuf::from("z-link"),
            target: b"a/000000".to_vec(),
        });
    }
    expected.push(FilesystemEntry::Directory {
        path: PathBuf::new(),
    });
    let root = FsRoot::open_read(directory.path()).await.unwrap();
    let cached = AtomicUsize::new(0);
    let read = AtomicUsize::new(0);
    let mut pages = walk_bounded_pages_excluding(
        &root,
        |identities| {
            let cached = &cached;
            async move {
                Ok(identities
                    .into_iter()
                    .enumerate()
                    .map(|(index, identity)| {
                        if identity.is_some() && index.is_multiple_of(3) {
                            cached.fetch_add(1, Ordering::SeqCst);
                            Some((1, digest))
                        } else {
                            None
                        }
                    })
                    .collect())
            }
        },
        |_, _| async {
            read.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok((1, false, digest))
        },
        2000,
        Some(PathBuf::from("a/000005")),
        std::num::NonZeroUsize::new(16).unwrap(),
    );
    let mut observed = Vec::new();
    let mut page_count = 0;
    while let Some(page) = pages.try_next().await.unwrap() {
        assert!(page.len() <= WALK_PAGE_ENTRIES);
        observed.extend(page);
        page_count += 1;
    }
    assert_eq!(page_count, 2);
    assert_eq!(observed, expected);
    assert_eq!(
        read.load(Ordering::SeqCst) + cached.load(Ordering::SeqCst),
        1039
    );
}

#[tokio::test]
async fn slow_first_file_does_not_block_admission_but_blocks_page_publication() {
    let (directory, _) = fixture(17);
    let root = FsRoot::open_read(directory.path()).await.unwrap();
    let last_started = tokio::sync::Notify::new();
    let release_first = tokio::sync::Notify::new();
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let mut pages = walk_bounded_pages_excluding(
        &root,
        |identities| async move { Ok(vec![None; identities.len()]) },
        |path, _| {
            let active = &active;
            let peak = &peak;
            let completed = &completed;
            let release_first = &release_first;
            let last_started = &last_started;
            async move {
                let _read = ActiveRead::enter(active, peak);
                if path == std::path::Path::new("000000") {
                    release_first.notified().await;
                }
                if path == std::path::Path::new("000016") {
                    last_started.notify_one();
                }
                completed.fetch_add(1, Ordering::SeqCst);
                Ok((1, false, BlobId::new(blake3::hash(b"x").into())))
            }
        },
        100,
        None,
        std::num::NonZeroUsize::new(16).unwrap(),
    );
    let next = pages.try_next();
    tokio::pin!(next);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut next => panic!("page was published before the first file completed"),
            _ = last_started.notified() => {},
        }
        assert_eq!(completed.load(Ordering::SeqCst), 16);
        assert_eq!(active.load(Ordering::SeqCst), 1);
        assert!(peak.load(Ordering::SeqCst) <= 16);
        release_first.notify_one();
        let page = next.await.unwrap().unwrap();
        let paths: Vec<_> = page
            .iter()
            .map(|entry| match entry {
                FilesystemEntry::Regular { path, .. } | FilesystemEntry::Directory { path } => {
                    path.clone()
                }
                _ => panic!("unexpected symlink"),
            })
            .collect();
        let expected: Vec<_> = (0..17)
            .map(|index| PathBuf::from(format!("{index:06}")))
            .chain(std::iter::once(PathBuf::new()))
            .collect();
        assert_eq!(paths, expected);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_or_cancelled_page_drops_outstanding_ingests() {
    let (directory, _) = fixture(17);
    let root = FsRoot::open_read(directory.path()).await.unwrap();
    for fail in [true, false] {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let first_started = tokio::sync::Notify::new();
        let mut pages = walk_bounded_pages_excluding(
            &root,
            |identities| async move { Ok(vec![None; identities.len()]) },
            |path, _| {
                let active = &active;
                let peak = &peak;
                let first_started = &first_started;
                async move {
                    let _read = ActiveRead::enter(active, peak);
                    if fail && path == std::path::Path::new("000001") {
                        return Err(Error::from("injected read failure"));
                    }
                    first_started.notify_one();
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            },
            100,
            None,
            std::num::NonZeroUsize::new(16).unwrap(),
        );
        if fail {
            let result = tokio::time::timeout(Duration::from_secs(5), pages.try_next())
                .await
                .unwrap();
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("injected read failure")
            );
        } else {
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    _ = pages.try_next() => panic!("pending reads unexpectedly completed"),
                    _ = first_started.notified() => {},
                }
            })
            .await
            .unwrap();
            assert!(active.load(Ordering::SeqCst) > 0);
        }
        drop(pages);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(peak.load(Ordering::SeqCst) <= 16);
    }
}

/// A permanent scheduling-only fixture: real handle-rooted reads and hashes,
/// optional controlled read latency, and exact post-order output validation.
#[tokio::test]
#[ignore = "run through benchmark run ingest-scheduling"]
async fn benchmark_ingest_scheduling() {
    let files: usize = std::env::var("CASITA_INGEST_FILES")
        .unwrap()
        .parse()
        .unwrap();
    let concurrency: std::num::NonZeroUsize = std::env::var("CASITA_INGEST_CONCURRENCY")
        .unwrap()
        .parse()
        .unwrap();
    let pattern = std::env::var("CASITA_INGEST_PATTERN").unwrap();
    let mode = std::env::var("CASITA_INGEST_MODE").unwrap();
    assert!(files > 0);
    assert!(matches!(pattern.as_str(), "uniform" | "skewed"));
    let scheduling = match mode.as_str() {
        "ordered" => CompletionOrder::Input,
        "ready" => CompletionOrder::Ready,
        _ => panic!("unknown completion order"),
    };
    let (directory, expected) = fixture(files);
    let root = FsRoot::open_read(directory.path()).await.unwrap();
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let started = std::time::Instant::now();
    let mut pages = walk_pages(
        &root,
        |identities| async move { Ok(vec![None; identities.len()]) },
        |path, _| {
            let root = &root;
            let pattern = &pattern;
            let active = &active;
            let peak = &peak;
            async move {
                let _read = ActiveRead::enter(active, peak);
                let index: usize = path.to_str().unwrap().parse().unwrap();
                if pattern == "skewed" {
                    tokio::time::sleep(Duration::from_millis(if index.is_multiple_of(16) {
                        20
                    } else {
                        1
                    }))
                    .await;
                }
                let (mut file, _) = root.open_file(&path).await?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).await?;
                Ok((
                    bytes.len() as u64,
                    false,
                    BlobId::new(blake3::hash(&bytes).into()),
                ))
            }
        },
        files + 1,
        None,
        concurrency,
        scheduling,
    );
    let mut largest_page = 0;
    let mut observed = Vec::new();
    while let Some(page) = pages.try_next().await.unwrap() {
        largest_page = largest_page.max(page.len());
        observed.extend(page);
    }
    let wall_nanos = started.elapsed().as_nanos() as u64;
    assert_eq!(observed, expected);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert!(peak.load(Ordering::SeqCst) <= concurrency.get());
    assert!(largest_page <= WALK_PAGE_ENTRIES);
    println!(
        "ingest_scheduling_sample {}",
        serde_json::json!({
            "files": files, "concurrency": concurrency.get(), "pattern": pattern, "mode": mode,
            "wall_nanos": wall_nanos, "peak_active": peak.load(Ordering::SeqCst),
            "largest_page": largest_page, "page_limit": WALK_PAGE_ENTRIES,
            "correctness": "exact post-order paths, sizes and digests; bounded active reads and pages",
        })
    );
}
