//! The handle-rooted filesystem primitive shared by import and materialization.
//!
//! Every read an importer performs and every write a checkout performs happens
//! through [`FsRoot`], which owns an open handle to the caller-selected root
//! directory. Descendants are named by paths relative to that handle and are
//! resolved one component at a time against it, so a component swapped for a
//! link after casita looked at it cannot redirect the operation outside the
//! root. The containment boundary is exactly the opened root:
//!
//! - **Unix**: descendants resolve through `openat`, with `openat2`
//!   `RESOLVE_BENEATH` where the kernel provides it; `..`, absolute paths, and
//!   symlinks that would leave the root are refused.
//! - **Windows**: descendants resolve through handle-relative opens that treat
//!   reparse points (symlinks and junctions alike) as boundaries rather than
//!   following them silently.
//!
//! Two things are deliberately outside the boundary. The path *to* the root is
//! the caller's own authority, resolved with ambient permissions, so casita
//! only insists that the root itself is a real directory and not a link. And a
//! link that legitimately lives inside a completed tree is data: casita writes
//! it, but a later consumer that follows it is following its own path, not
//! casita's.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::DirExt;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};
use futures::stream::BoxStream;

use crate::error::Error;

/// An open directory handle that contains every operation beneath it.
#[derive(Clone, Debug)]
pub(crate) struct FsRoot {
    dir: Arc<Dir>,
    /// The path the handle was opened from, for error messages only. Operations
    /// never re-resolve through it.
    path: PathBuf,
}

impl FsRoot {
    /// Open an existing directory as a root for reading.
    pub(crate) async fn open_read(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open(path, false).await
    }

    /// Open a directory as a root for writing, creating it if it is absent.
    ///
    /// Creation is path-based, like any other caller-authorized directory
    /// creation; containment starts at the handle this returns.
    pub(crate) async fn open_write(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open(path, true).await
    }

    #[tracing::instrument(
        name = "filesystem.root.open",
        level = "debug",
        skip_all,
        fields(create = create)
    )]
    async fn open(path: impl AsRef<Path>, create: bool) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let opened = path.clone();
        let dir = tokio::task::spawn_blocking(move || -> Result<Dir, Error> {
            prepare_root(&opened, create)?;
            open_root_nofollow(&opened)
        })
        .await??;
        Ok(Self {
            dir: Arc::new(dir),
            path,
        })
    }

    /// The path the root handle was opened from.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the root holds no entries at all.
    pub(crate) async fn is_empty(&self) -> Result<bool, Error> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || -> Result<bool, Error> {
            Ok(dir.entries()?.next().is_none())
        })
        .await?
    }

    /// Create one directory under the root. Intermediate components must
    /// already exist, which they do for a materialization that creates parents
    /// before children.
    pub(crate) async fn create_dir(&self, relative: impl AsRef<Path>) -> Result<(), Error> {
        let dir = self.dir.clone();
        let relative = relative.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            dir.create_dir(&relative)?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    /// Create a new regular file under the root, failing if it already exists.
    pub(crate) async fn create_file(
        &self,
        relative: impl AsRef<Path>,
        executable: bool,
    ) -> Result<tokio::fs::File, Error> {
        let dir = self.dir.clone();
        let relative = relative.as_ref().to_path_buf();
        let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, Error> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            // A create_new open cannot follow a link into an existing file, but
            // stating the intent keeps the guarantee independent of that.
            options.follow(FollowSymlinks::No);
            #[cfg(unix)]
            options.mode(if executable { 0o777 } else { 0o666 });
            #[cfg(not(unix))]
            let _ = executable;
            Ok(dir.open_with(&relative, &options)?.into_std())
        })
        .await??;
        Ok(tokio::fs::File::from_std(file))
    }

    /// Create a symlink under the root holding `target` verbatim.
    ///
    /// The link itself is placed through the root handle, but `target` is link
    /// data rather than a path casita resolves: a stored tree may name anything,
    /// including something outside the root, and materializing the link does
    /// not follow it. Windows is the exception, because it refuses to create a
    /// handle-relative link to an absolute target at all.
    ///
    /// `directory_target` selects the Windows link flavor and is ignored on
    /// Unix, where links carry no such distinction.
    pub(crate) async fn symlink(
        &self,
        relative: impl AsRef<Path>,
        target: &Path,
        directory_target: bool,
    ) -> Result<(), Error> {
        let dir = self.dir.clone();
        let relative = relative.as_ref().to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            #[cfg(windows)]
            {
                if target.has_root() {
                    return Err(Error::from(format!(
                        "cannot materialize the link {} on Windows: its target {} is absolute, \
                         and a handle-rooted checkout only creates relative links",
                        relative.display(),
                        target.display()
                    )));
                }
                if directory_target {
                    dir.symlink_dir(&target, &relative)?;
                } else {
                    dir.symlink_file(&target, &relative)?;
                }
            }
            #[cfg(not(windows))]
            {
                let _ = directory_target;
                // `symlink_contents` stores the target as given; `symlink`
                // would refuse an absolute one, which a stored tree may name.
                dir.symlink_contents(&target, &relative)?;
            }
            Ok(())
        })
        .await??;
        Ok(())
    }

    /// Whether `relative` names a directory, following links contained by the
    /// root. Anything unreachable, escaping, or absent answers `false`.
    ///
    /// Only Windows needs this: it fixes a symlink's flavor at creation time,
    /// while Unix links carry no such distinction.
    #[cfg(windows)]
    pub(crate) async fn is_directory(&self, relative: impl AsRef<Path>) -> bool {
        let dir = self.dir.clone();
        let relative = relative.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || {
            dir.metadata(&relative)
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }

    /// Open a regular file under the root without following a link in the final
    /// component, returning the open file and its metadata.
    ///
    /// Anything that is not a regular file is refused: the caller ingests file
    /// contents, and a link, device, socket, or fifo has no payload identity.
    pub(crate) async fn open_file(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<(tokio::fs::File, std::fs::Metadata), Error> {
        let dir = self.dir.clone();
        let relative = relative.as_ref().to_path_buf();
        let (file, metadata) =
            tokio::task::spawn_blocking(move || -> Result<(std::fs::File, _), Error> {
                let mut options = OpenOptions::new();
                options.read(true);
                options.follow(FollowSymlinks::No);
                // A fifo or device that survived the file-type check would
                // otherwise block the open until a writer appears.
                #[cfg(unix)]
                options.custom_flags(libc::O_NONBLOCK);
                let file = dir.open_with(&relative, &options)?.into_std();
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Err(Error::from(format!(
                        "{} is not a regular file",
                        relative.display()
                    )));
                }
                Ok((file, metadata))
            })
            .await??;
        Ok((tokio::fs::File::from_std(file), metadata))
    }

    /// Walk the root in post-order without following links.
    #[allow(dead_code)] // Used by focused FsRoot tests and available to callers without exclusions.
    pub(crate) async fn walk_post_order(
        &self,
        max_entries: usize,
    ) -> Result<Vec<RootedEntry>, Error> {
        self.walk_post_order_excluding(max_entries, None).await
    }

    /// Walk the root in post-order without following links, omitting one exact
    /// relative path (and an entire subtree when it names a directory).
    pub(crate) async fn walk_post_order_excluding(
        &self,
        max_entries: usize,
        excluded: Option<PathBuf>,
    ) -> Result<Vec<RootedEntry>, Error> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || walk_blocking(&dir, max_entries, excluded.as_deref()))
            .await?
    }

    /// Walk separate handles with shared pages. Indices only route entries;
    /// no synthetic filesystem parent or symlink resolution is involved.
    pub(crate) fn walk_forest_pages(
        roots: &[(Self, Option<PathBuf>)],
        max_entries: usize,
        page_entries: usize,
    ) -> BoxStream<'static, Result<Vec<(usize, RootedEntry)>, Error>> {
        let roots: Vec<_> = roots
            .iter()
            .map(|(root, excluded)| (root.dir.clone(), excluded.clone()))
            .collect();
        Box::pin(async_stream::try_stream! {
            if page_entries == 0 {
                Err(Error::from("filesystem walk page size must be nonzero"))?;
            }
            let mut remaining = roots.into_iter().enumerate();
            let mut active: Option<(usize, BlockingWalk)> = None;
            let mut emitted = 0;
            loop {
                let (next_roots, next_active, next_emitted, page) = tokio::task::spawn_blocking(move || {
                    let page = (|| -> Result<_, Error> {
                        let mut page = Vec::with_capacity(page_entries);
                        while page.len() < page_entries {
                            if active.is_none() {
                                let Some((index, (root, excluded))) = remaining.next() else { break };
                                active = Some((index, BlockingWalk::new(&root, max_entries - emitted, excluded.as_deref())?));
                            }
                            let (index, walk) = active.as_mut().unwrap();
                            let entries = walk.next_page(page_entries - page.len())?;
                            emitted += entries.len();
                            let finished = walk.stack.is_empty();
                            page.extend(entries.into_iter().map(|entry| (*index, entry)));
                            if finished { active = None; }
                        }
                        Ok(page)
                    })();
                    (remaining, active, emitted, page)
                }).await?;
                remaining = next_roots;
                active = next_active;
                emitted = next_emitted;
                let page = page?;
                if page.is_empty() { break; }
                yield page;
            }
        })
    }

    /// Stream a stable post-order walk in bounded pages.
    ///
    /// Each blocking step retains only the directory frames on the active DFS
    /// branch and at most `page_entries` completed entries. This is the import
    /// primitive for repositories whose trees are too large to inventory in
    /// memory before the first durable checkpoint.
    pub(crate) fn walk_post_order_pages_excluding(
        &self,
        max_entries: usize,
        excluded: Option<PathBuf>,
        page_entries: usize,
    ) -> BoxStream<'static, Result<Vec<RootedEntry>, Error>> {
        let dir = self.dir.clone();
        Box::pin(async_stream::try_stream! {
            if page_entries == 0 {
                Err(Error::from("filesystem walk page size must be nonzero"))?;
            }
            let mut walk = tokio::task::spawn_blocking(move || {
                BlockingWalk::new(&dir, max_entries, excluded.as_deref())
            })
            .await??;
            loop {
                let (next, page) = tokio::task::spawn_blocking(move || {
                    let page = walk.next_page(page_entries);
                    (walk, page)
                })
                .await?;
                walk = next;
                let page = page?;
                if page.is_empty() {
                    break;
                }
                yield page;
            }
        })
    }
}

/// Create the caller-selected root if it is absent.
///
/// The subsequent no-follow acquisition decides whether the final component is
/// safe to use. This operation and its ancestors are caller authority.
fn prepare_root(path: &Path, create: bool) -> Result<(), Error> {
    if create {
        match std::fs::create_dir_all(path) {
            Ok(()) => {}
            // An existing entry is fine; the no-follow open below decides
            // whether it is a directory casita may use as its root.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Open the selected root relative to an already-open parent directory.
///
/// `Dir::open_ambient_dir(path)` would re-resolve and follow a final symlink.
/// Splitting the path makes `open_dir_nofollow` the single operation that
/// chooses the root handle, closing the check-then-open race.
fn open_root_nofollow(path: &Path) -> Result<Dir, Error> {
    let (parent_path, name) = root_parent_and_name(path);
    let parent = Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    let root = parent.open_dir_nofollow(name)?;
    validate_open_root(&root, path)?;
    Ok(root)
}

/// Split a root path into its caller-authorized parent and its untrusted final
/// component. Roots such as `.` and `/` are opened as `.` beneath their own
/// stable ambient directory handle.
fn root_parent_and_name(path: &Path) -> (&Path, &OsStr) {
    if let Some(name) = path.file_name() {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        (parent, name)
    } else if path.has_root() {
        (path, OsStr::new("."))
    } else {
        (Path::new("."), OsStr::new("."))
    }
}

/// Check the already-open root rather than a pathname that could be swapped.
fn validate_open_root(root: &Dir, path: &Path) -> Result<(), Error> {
    let metadata = root.metadata(".")?;
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(Error::from(format!(
            "{} is a link rather than a directory; casita only contains writes \\
             beneath a real directory root",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(Error::from(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    Ok(())
}

/// What a walk observed about a file, enough to recognize it unchanged.
///
/// The walk already stats every entry to classify it, so carrying this along
/// costs no extra syscall. `ctime` is included because userspace cannot set it,
/// which catches content rewritten with a restored modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) size: u64,
    pub(crate) mtime_sec: i64,
    pub(crate) mtime_nsec: i64,
    pub(crate) ctime_sec: i64,
    pub(crate) ctime_nsec: i64,
    pub(crate) executable: bool,
}

/// Read a file's identity from the metadata the walk already collected.
///
/// Only implemented where the platform exposes a stable device and inode pair.
/// Elsewhere every file reads as unrecognizable, which costs a re-read and
/// never a wrong answer.
#[cfg(unix)]
fn file_identity(metadata: &cap_std::fs::Metadata) -> Option<FileIdentity> {
    use cap_std::fs::MetadataExt;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
        executable: metadata.mode() & 0o100 != 0,
    })
}

#[cfg(not(unix))]
fn file_identity(_metadata: &cap_std::fs::Metadata) -> Option<FileIdentity> {
    None
}

impl RootedEntry {
    /// Whether this entry is a regular file.
    pub(crate) fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }
}

/// One entry of a handle-rooted walk, named relative to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootedEntry {
    Directory {
        path: PathBuf,
    },
    File {
        path: PathBuf,
        /// Absent where the platform cannot identify a file cheaply.
        identity: Option<FileIdentity>,
    },
    Symlink {
        path: PathBuf,
        /// The stored link text, read through the parent's handle.
        target: PathBuf,
    },
}

/// One directory being iterated, with the handle its children resolve against.
struct WalkFrame {
    path: PathBuf,
    dir: Dir,
    children: Vec<std::ffi::OsString>,
    next: usize,
}

/// Incremental depth-first post-order walk holding only the active branch.
struct BlockingWalk {
    stack: Vec<WalkFrame>,
    max_entries: usize,
    emitted: usize,
    excluded: Option<PathBuf>,
}

impl BlockingWalk {
    fn frame(path: PathBuf, dir: Dir) -> Result<WalkFrame, Error> {
        let mut children = Vec::new();
        for entry in dir.entries()? {
            children.push(entry?.file_name());
        }
        // A stable order keeps a walk reproducible across filesystems; the
        // canonical directory encoding sorts by name anyway.
        children.sort();
        Ok(WalkFrame {
            path,
            dir,
            children,
            next: 0,
        })
    }

    fn new(root: &Dir, max_entries: usize, excluded: Option<&Path>) -> Result<Self, Error> {
        Ok(Self {
            stack: vec![Self::frame(PathBuf::new(), root.try_clone()?)?],
            max_entries,
            emitted: 0,
            excluded: excluded.map(Path::to_path_buf),
        })
    }

    fn push(&mut self, entries: &mut Vec<RootedEntry>, entry: RootedEntry) -> Result<(), Error> {
        if self.emitted >= self.max_entries {
            return Err(Error::from(format!(
                "filesystem walk exceeds {} entries",
                self.max_entries
            )));
        }
        self.emitted += 1;
        entries.push(entry);
        Ok(())
    }

    fn next_page(&mut self, page_entries: usize) -> Result<Vec<RootedEntry>, Error> {
        let mut entries = Vec::with_capacity(page_entries);
        while entries.len() < page_entries {
            let Some(top) = self.stack.last_mut() else {
                break;
            };
            if top.next == top.children.len() {
                let finished = self.stack.pop().expect("the loop borrowed the top frame");
                self.push(
                    &mut entries,
                    RootedEntry::Directory {
                        path: finished.path,
                    },
                )?;
                continue;
            }
            let name = top.children[top.next].clone();
            top.next += 1;
            let path = top.path.join(&name);
            if self
                .excluded
                .as_deref()
                .is_some_and(|excluded| excluded == path)
            {
                continue;
            }
            // `file_type` comes from the directory listing, so it never follows.
            let metadata = match top.dir.symlink_metadata(&name) {
                Ok(metadata) => metadata,
                // An entry unlinked between listing and stat is not part of the
                // tree; a concurrently mutated source is the caller's problem,
                // but it must not abort the import with a spurious error.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let file_type = metadata.file_type();

            if file_type.is_symlink() {
                // The stored text, not a resolved path: a link in an imported
                // tree may legitimately name something absolute or outside the root.
                let target = top.dir.read_link_contents(&name)?;
                self.push(&mut entries, RootedEntry::Symlink { path, target })?;
            } else if file_type.is_dir() {
                let child = top.dir.open_dir(&name)?;
                self.stack.push(Self::frame(path, child)?);
            } else if file_type.is_file() {
                self.push(
                    &mut entries,
                    RootedEntry::File {
                        path,
                        identity: file_identity(&metadata),
                    },
                )?;
            }
            // Anything else (device, socket, fifo) has no representation in
            // the canonical filesystem model and is skipped, as before.
        }
        Ok(entries)
    }
}

/// Collecting compatibility wrapper for focused callers and tests.
fn walk_blocking(
    root: &Dir,
    max_entries: usize,
    excluded: Option<&Path>,
) -> Result<Vec<RootedEntry>, Error> {
    let mut walk = BlockingWalk::new(root, max_entries, excluded)?;
    let mut entries = Vec::new();
    loop {
        let page = walk.next_page(1_024)?;
        if page.is_empty() {
            break;
        }
        entries.extend(page);
    }
    Ok(entries)
}

/// Windows junctions are reparse points that are not symlinks, so the file
/// attributes decide rather than the file type alone.
#[cfg(windows)]
fn is_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &cap_std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a directory link named `link` pointing at `target`.
    ///
    /// Unix uses a symlink; Windows uses a junction, which needs no elevated
    /// privileges and is the reparse point an attacker there actually has.
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .status()
                .unwrap();
            assert!(status.success(), "creating a junction failed");
        }
    }

    /// A root directory plus an "outside" directory no operation may reach.
    fn playground() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"not yours").unwrap();
        (temp, root, outside)
    }

    #[tokio::test]
    async fn a_linked_root_is_refused() {
        let (_temp, root, outside) = playground();
        let link = root.parent().unwrap().join("link");
        link_dir(&outside, &link);

        assert!(FsRoot::open_write(&link).await.is_err());
        assert!(FsRoot::open_read(&link).await.is_err());
        // and nothing was created through it.
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[test]
    fn a_root_swapped_before_handle_acquisition_is_refused() {
        let (_temp, root, outside) = playground();
        // Let the production setup create the root, then replace it in the
        // precise interval that used to lie between root validation and the
        // ambient directory open.
        std::fs::remove_dir(&root).unwrap();
        let parked = root.with_file_name("root-before-acquisition");
        prepare_root(&root, true).unwrap();
        std::fs::rename(&root, &parked).unwrap();
        link_dir(&outside, &root);
        let result = open_root_nofollow(&root);

        assert!(result.is_err());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"not yours");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn a_file_root_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert!(FsRoot::open_read(&file).await.is_err());
    }

    #[tokio::test]
    async fn an_ancestor_replaced_after_creation_cannot_redirect_a_write() {
        let (_temp, root, outside) = playground();
        let handle = FsRoot::open_write(&root).await.unwrap();
        handle.create_dir("nested").await.unwrap();

        // The classic swap: the directory casita just created becomes a link
        // to somewhere else before the file underneath it is written.
        std::fs::remove_dir(root.join("nested")).unwrap();
        link_dir(&outside, &root.join("nested"));

        assert!(handle.create_file("nested/planted", false).await.is_err());
        assert!(!outside.join("planted").exists());
        assert!(handle.create_dir("nested/planted").await.is_err());
        assert!(!outside.join("planted").exists());
        assert!(
            handle
                .symlink("nested/planted", Path::new("elsewhere"), false)
                .await
                .is_err()
        );
        assert!(!outside.join("planted").exists());
    }

    #[tokio::test]
    async fn an_ancestor_replaced_after_the_walk_cannot_redirect_a_read() {
        let (_temp, root, outside) = playground();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/data"), b"mine").unwrap();

        let handle = FsRoot::open_read(&root).await.unwrap();
        let walked = handle.walk_post_order(16).await.unwrap();
        assert!(walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::File { path, .. } if path == Path::new("nested/data")
        )));

        // Between the walk and the read, the directory becomes a link.
        std::fs::remove_file(root.join("nested/data")).unwrap();
        std::fs::remove_dir(root.join("nested")).unwrap();
        link_dir(&outside, &root.join("nested"));

        assert!(handle.open_file("nested/secret").await.is_err());
    }

    #[tokio::test]
    async fn escaping_paths_are_refused() {
        let (_temp, root, _outside) = playground();
        let handle = FsRoot::open_write(&root).await.unwrap();
        assert!(handle.create_dir("../escaped").await.is_err());
        assert!(handle.create_file("../escaped", false).await.is_err());
        assert!(handle.open_file("../outside/secret").await.is_err());
        assert!(!root.parent().unwrap().join("escaped").exists());
    }

    #[tokio::test]
    async fn a_walk_reports_a_linked_directory_without_descending() {
        let (_temp, root, outside) = playground();
        link_dir(&outside, &root.join("elsewhere"));
        std::fs::write(root.join("own"), b"mine").unwrap();

        let handle = FsRoot::open_read(&root).await.unwrap();
        let walked = handle.walk_post_order(16).await.unwrap();

        // The link is an entry of its own; nothing below it was read. A
        // junction and a symlink both report as links here.
        assert!(walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::Symlink { path, .. } if path == Path::new("elsewhere")
        )));
        assert!(!walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::File { path, .. } if path.starts_with("elsewhere")
        )));
        assert!(walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::File { path, .. } if path == Path::new("own")
        )));
    }

    #[tokio::test]
    async fn a_walk_is_post_order_and_bounded() {
        let (_temp, root, _outside) = playground();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/deep"), b"deep").unwrap();

        let handle = FsRoot::open_read(&root).await.unwrap();
        let walked = handle.walk_post_order(16).await.unwrap();
        let shape: Vec<(&str, PathBuf)> = walked
            .iter()
            .map(|entry| match entry {
                RootedEntry::File { path, .. } => ("file", path.clone()),
                RootedEntry::Directory { path } => ("dir", path.clone()),
                RootedEntry::Symlink { path, .. } => ("link", path.clone()),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("file", PathBuf::from("a/b/deep")),
                ("dir", PathBuf::from("a/b")),
                ("dir", PathBuf::from("a")),
                ("dir", PathBuf::new()),
            ]
        );
        assert!(handle.walk_post_order(3).await.is_err());
    }

    #[tokio::test]
    async fn a_walk_can_exclude_one_control_path() {
        let (_temp, root, _outside) = playground();
        std::fs::write(root.join(".casita"), b"workspace marker").unwrap();
        std::fs::write(root.join("included"), b"project content").unwrap();

        let handle = FsRoot::open_read(&root).await.unwrap();
        let walked = handle
            .walk_post_order_excluding(16, Some(PathBuf::from(".casita")))
            .await
            .unwrap();
        assert!(walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::File { path, .. } if path == Path::new("included")
        )));
        assert!(!walked.iter().any(|entry| matches!(
            entry,
            RootedEntry::File { path, .. } if path == Path::new(".casita")
        )));
    }

    #[tokio::test]
    async fn empty_reporting_and_file_modes_survive_the_handle() {
        let (_temp, root, _outside) = playground();
        let handle = FsRoot::open_write(&root).await.unwrap();
        assert!(handle.is_empty().await.unwrap());

        let mut file = handle.create_file("script", true).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut file, b"#!/bin/sh\n")
            .await
            .unwrap();
        drop(file);
        assert!(!handle.is_empty().await.unwrap());

        let (_, metadata) = handle.open_file("script").await.unwrap();
        assert_eq!(crate::filesystem::is_executable(&metadata), cfg!(unix));
        // A second create of the same name conflicts rather than truncating.
        assert!(handle.create_file("script", false).await.is_err());
    }
}
