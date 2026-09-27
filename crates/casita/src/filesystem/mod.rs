//! Filesystem ingestion, checkout, handle-rooted access, and local ingest caching.
//!
//! Discovery and ingestion both go through [`FsRoot`](crate::filesystem::root::FsRoot):
//! the walk descends through directory handles and every file is opened
//! relative to the root handle, so no read escapes the imported tree even when
//! another process rewrites a component underneath it.

pub(crate) mod cache;
pub(crate) mod checkout;
pub(crate) mod names;
pub(crate) mod root;

use std::future::Future;
use std::path::PathBuf;

use futures::stream::{self, BoxStream, StreamExt, TryStreamExt};

use crate::BlobId;
use crate::error::Error;
use crate::filesystem::root::{FileIdentity, FsRoot, RootedEntry};

/// One post-order entry produced by a filesystem walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilesystemEntry {
    Regular {
        path: PathBuf,
        size: u64,
        executable: bool,
        digest: BlobId,
    },
    Symlink {
        path: PathBuf,
        target: Vec<u8>,
    },
    Directory {
        path: PathBuf,
    },
}

/// How many regular files are ingested concurrently during a walk.
pub(crate) const DEFAULT_FILE_CONCURRENCY: std::num::NonZeroUsize =
    std::num::NonZeroUsize::new(16).unwrap();

/// Entries inventoried and recognized at once. This bounds the walk, ingest
/// cache, and prepared-entry allocations independently of total tree size.
pub(crate) const WALK_PAGE_ENTRIES: usize = 1_024;

/// Read buffer for streaming a file into physical payload storage.
pub(crate) const FILE_READ_BUFFER_SIZE: usize = 256 * 1024;

#[derive(Clone, Copy)]
enum CompletionOrder {
    Ready,
    /// Retain the previous scheduler as a permanent benchmark control.
    #[cfg(test)]
    Input,
}

/// Stream `root` in bounded post-order pages without following links.
///
/// `ingest_file` receives the root handle and the path of the file relative to
/// it, never an absolute path: opening it any other way would reintroduce the
/// window the handle closes. `recognize` is offered each page's files in one
/// batch and answers which already have durable content. Those files are never
/// opened. No allocation in this adapter grows with the complete tree.
#[cfg(test)]
pub(crate) fn walk_bounded_pages_excluding<'a, F, Fut, R, RFut>(
    root: &'a FsRoot,
    recognize: R,
    ingest_file: F,
    max_entries: usize,
    excluded: Option<PathBuf>,
    concurrency: std::num::NonZeroUsize,
) -> BoxStream<'a, Result<Vec<FilesystemEntry>, Error>>
where
    R: Fn(Vec<Option<FileIdentity>>) -> RFut + Send + Sync + 'a,
    RFut: Future<Output = Result<Vec<Option<(u64, BlobId)>>, Error>> + Send,
    F: Fn(PathBuf, Option<FileIdentity>) -> Fut + Send + Sync + 'a,
    Fut: Future<Output = Result<(u64, bool, BlobId), Error>> + Send,
{
    walk_pages(
        root,
        recognize,
        ingest_file,
        max_entries,
        excluded,
        concurrency,
        CompletionOrder::Ready,
    )
}

#[cfg(test)]
fn walk_pages<'a, F, Fut, R, RFut>(
    root: &'a FsRoot,
    recognize: R,
    ingest_file: F,
    max_entries: usize,
    excluded: Option<PathBuf>,
    concurrency: std::num::NonZeroUsize,
    completion_order: CompletionOrder,
) -> BoxStream<'a, Result<Vec<FilesystemEntry>, Error>>
where
    R: Fn(Vec<Option<FileIdentity>>) -> RFut + Send + Sync + 'a,
    RFut: Future<Output = Result<Vec<Option<(u64, BlobId)>>, Error>> + Send,
    F: Fn(PathBuf, Option<FileIdentity>) -> Fut + Send + Sync + 'a,
    Fut: Future<Output = Result<(u64, bool, BlobId), Error>> + Send,
{
    let pages = root.walk_post_order_pages_excluding(max_entries, excluded, WALK_PAGE_ENTRIES);
    let pages = Box::pin(pages.map_ok(|page| page.into_iter().map(|entry| (0, entry)).collect()));
    Box::pin(
        ingest_pages(
            pages,
            recognize,
            move |_, path, identity| ingest_file(path, identity),
            concurrency,
            completion_order,
        )
        .map_ok(|page| page.into_iter().map(|(_, entry)| entry).collect()),
    )
}

pub(crate) fn walk_import_pages<'a, F, Fut, R, RFut>(
    roots: &[(FsRoot, Option<PathBuf>)],
    recognize: R,
    ingest_file: F,
    max_entries: usize,
    concurrency: std::num::NonZeroUsize,
    forest: bool,
    traversal_nanos: &'a std::sync::atomic::AtomicU64,
) -> BoxStream<'a, Result<Vec<(usize, FilesystemEntry)>, Error>>
where
    R: Fn(Vec<Option<FileIdentity>>) -> RFut + Send + Sync + 'a,
    RFut: Future<Output = Result<Vec<Option<(u64, BlobId)>>, Error>> + Send,
    F: Fn(usize, PathBuf, Option<FileIdentity>) -> Fut + Send + Sync + 'a,
    Fut: Future<Output = Result<(u64, bool, BlobId), Error>> + Send,
{
    let mut pages = if forest {
        FsRoot::walk_forest_pages(roots, max_entries, WALK_PAGE_ENTRIES)
    } else {
        assert_eq!(roots.len(), 1);
        Box::pin(
            roots[0]
                .0
                .walk_post_order_pages_excluding(max_entries, roots[0].1.clone(), WALK_PAGE_ENTRIES)
                .map_ok(|page| page.into_iter().map(|entry| (0, entry)).collect()),
        )
    };
    let timed = Box::pin(async_stream::try_stream! {
        loop {
            let started = std::time::Instant::now();
            let page = pages.try_next().await?;
            traversal_nanos.fetch_add(started.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
            let Some(page) = page else { break };
            yield page;
        }
    });
    ingest_pages(
        timed,
        recognize,
        ingest_file,
        concurrency,
        CompletionOrder::Ready,
    )
}

fn ingest_pages<'a, F, Fut, R, RFut>(
    mut pages: BoxStream<'a, Result<Vec<(usize, RootedEntry)>, Error>>,
    recognize: R,
    ingest_file: F,
    concurrency: std::num::NonZeroUsize,
    completion_order: CompletionOrder,
) -> BoxStream<'a, Result<Vec<(usize, FilesystemEntry)>, Error>>
where
    R: Fn(Vec<Option<FileIdentity>>) -> RFut + Send + Sync + 'a,
    RFut: Future<Output = Result<Vec<Option<(u64, BlobId)>>, Error>> + Send,
    F: Fn(usize, PathBuf, Option<FileIdentity>) -> Fut + Send + Sync + 'a,
    Fut: Future<Output = Result<(u64, bool, BlobId), Error>> + Send,
{
    Box::pin(async_stream::try_stream! {
        while let Some(walked) = pages.try_next().await? {
            let walked_entries = walked.len();
            let identities: Vec<Option<FileIdentity>> = walked
                .iter()
                .filter_map(|(_, entry)| match entry {
                    RootedEntry::File { identity, .. } => Some(*identity),
                    _ => None,
                })
                .collect();
            let recognized = recognize(identities).await?;
            let recognized_files = recognized.iter().filter(|entry| entry.is_some()).count();
            tracing::debug!(
                walked_entries,
                recognized_files,
                "filesystem import page inventoried"
            );
            if recognized.len() != walked.iter().filter(|(_, entry)| entry.is_file()).count() {
                Err(Error::from(
                    "the ingest cache answered a different number of files than were walked",
                ))?;
            }
            let mut recognized = recognized.into_iter();
            let prepared: Vec<_> = walked
                .into_iter()
                .map(|(root_index, entry)| {
                    let known = match &entry {
                        RootedEntry::File { .. } => recognized.next().flatten(),
                        _ => None,
                    };
                    (root_index, entry, known)
                })
                .collect();
            let ingest_file = &ingest_file;
            let ingests = stream::iter(prepared)
                .enumerate()
                .map(|(index, (root_index, entry, known))| async move {
                    let entry = match entry {
                        RootedEntry::Directory { path } => Ok(FilesystemEntry::Directory { path }),
                        RootedEntry::Symlink { path, target } => Ok(FilesystemEntry::Symlink {
                            path,
                            target: crate::filesystem::names::os_str_bytes(target.as_os_str())?.to_vec(),
                        }),
                        RootedEntry::File { path, identity } => {
                            // A recognized file is never opened: its content is already
                            // stored under the digest recorded for exactly this
                            // device, inode, size and timestamps.
                            if let (Some((size, digest)), Some(identity)) = (known, identity) {
                                return Ok((index, (root_index, FilesystemEntry::Regular {
                                    path,
                                    size,
                                    executable: identity.executable,
                                    digest,
                                })));
                            }
                            let (size, executable, digest) =
                                ingest_file(root_index, path.clone(), identity).await?;
                            Ok::<_, Error>(FilesystemEntry::Regular {
                                path,
                                size,
                                executable,
                                digest,
                            })
                        }
                    }?;
                    Ok((index, (root_index, entry)))
                });
            // A completed file frees a slot immediately, even when an earlier
            // file is still reading. Restore post-order positions within this
            // already bounded page before directory construction/publication.
            let entries = match completion_order {
                CompletionOrder::Ready => collect_page(
                    ingests.buffer_unordered(concurrency.get()), walked_entries,
                ).await?,
                #[cfg(test)]
                CompletionOrder::Input => collect_page(
                    ingests.buffered(concurrency.get()), walked_entries,
                ).await?,
            };
            yield entries;
        }
    })
}

async fn collect_page<S, T>(mut completed: S, entries: usize) -> Result<Vec<T>, Error>
where
    T: Clone,
    S: futures::Stream<Item = Result<(usize, T), Error>> + Unpin,
{
    let mut ordered = vec![None; entries];
    while let Some((index, entry)) = completed.try_next().await? {
        ordered[index] = Some(entry);
    }
    Ok(ordered
        .into_iter()
        .map(|entry| entry.expect("every page entry completed"))
        .collect())
}

#[cfg(test)]
mod scheduling_tests;

/// Whether an open regular file should materialize as executable.
#[cfg(unix)]
pub(crate) fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o100 != 0
}

/// Windows has no executable bit in the repository's filesystem model.
#[cfg(windows)]
pub(crate) fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// What a repository remembers about a file it has already ingested.
///
/// This is a local accelerator, not repository state: it records nothing a
/// second machine could use and nothing a reader may rely on. A lookup that
/// answers `None` costs a re-read and never a wrong answer, so a backend is
/// free to forget entries whenever it likes.
#[async_trait::async_trait]
pub(crate) trait IngestCache: Send + Sync {
    /// The content each file held when last ingested, in input order.
    async fn recall(&self, files: &[FileIdentity]) -> Result<Vec<Option<BlobId>>, Error>;

    /// Remember the content these files hold now.
    async fn remember(&self, files: &[(FileIdentity, BlobId)]) -> Result<(), Error>;
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn file_concurrency_bounds_active_ingests_and_preserves_order() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..40 {
            std::fs::write(directory.path().join(format!("{index:02}")), b"file").unwrap();
        }
        let root = FsRoot::open_read(directory.path()).await.unwrap();
        let mut reference = None;
        for limit in [1, 2, 16, 64] {
            let active = AtomicUsize::new(0);
            let peak = AtomicUsize::new(0);
            let pages = walk_bounded_pages_excluding(
                &root,
                |identities| async move { Ok(vec![None; identities.len()]) },
                |_, _| async {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok((4, false, BlobId::new(blake3::hash(b"file").into())))
                },
                100,
                None,
                std::num::NonZeroUsize::new(limit).unwrap(),
            );
            let entries = pages.try_collect::<Vec<_>>().await.unwrap();
            assert_eq!(peak.load(Ordering::SeqCst), limit.min(40));
            assert_eq!(active.load(Ordering::SeqCst), 0);
            if let Some(reference) = &reference {
                assert_eq!(&entries, reference);
            } else {
                reference = Some(entries);
            }
        }
    }
}
