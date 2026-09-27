//! The FUSE transport for a [`FilesystemView`].
//!
//! [`StoreFs`] implements `fuse-backend-rs`' `FileSystem`, which is the shared
//! interface of that crate's `/dev/fuse` server and its virtio-fs server. The
//! `/dev/fuse` side is wired up here in [`FuseMount`]; a virtio-fs daemon for
//! VM builders would reuse [`StoreFs`] unchanged.
//!
//! Store content is read-only and content addressed, so content attributes
//! and directory entries have long lifetimes and open files keep the kernel's
//! page cache. Fixed views also cache their root entries. Shared input mounts
//! instead lease unique root names; their root lookups are never cached, so
//! adding or releasing a lease is visible immediately.

mod daemon;
mod inputs;
mod listings;
mod stats;

use std::collections::hash_map::Entry as MapEntry;
use std::collections::HashMap;
use std::ffi::CStr;
use std::io::{self, Cursor};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crate::ContentStream;
use fuse_backend_rs::abi::fuse_abi::{stat64, Attr, OpenOptions};
use fuse_backend_rs::api::filesystem::{
    Context, DirEntry, Entry, FileSystem, FsOptions, ZeroCopyWriter, ROOT_ID,
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tracing::warn;

use self::listings::{Listing, ListingCache as Cache};
use crate::{ContentKey, FilesystemEntry, FilesystemNode, FilesystemNodeKind, FilesystemView};

pub use daemon::FuseMount;
pub use inputs::{InputLease, InputRegistry};
pub use listings::ListingCache;
pub use stats::{MountStats, Op, StatsSnapshot};

/// Content addressed subtrees never change, so the kernel may cache them.
/// Shared mount roots use zero entry lifetimes instead. A decade rather than
/// `Duration::MAX`, which the kernel has to convert to jiffies: no mount
/// outlives this, and nothing can invalidate an answer anyway.
const FOREVER: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

/// An open file handle.
///
/// The blob behind it is not opened until the first read. A build execs
/// the same few binaries thousands of times, and while the kernel still
/// has their pages there is no read to serve, so there is nothing to open:
/// those execs then cost one round trip each and no store access at all.
///
/// Shared because the kernel may have several reads in flight on a handle,
/// and they take turns on the reader's seek position.
struct OpenBlob {
    node: FilesystemNode,
    reader: Option<Box<dyn ContentStream>>,
    runtime: tokio::runtime::Handle,
}

impl Drop for OpenBlob {
    fn drop(&mut self) {
        // RELEASE and unmount run on ordinary threads. Repository readers
        // schedule their pin cleanup when dropped, so enter the owning runtime
        // even when the last reference belonged to an in-flight read.
        let _entered = self.runtime.enter();
        drop(self.reader.take());
    }
}

type OpenFile = Arc<Mutex<OpenBlob>>;

/// A read-only FUSE filesystem serving one [`FilesystemView`].
pub struct StoreFs {
    view: FilesystemView,
    input_registry: Option<Arc<InputRegistry>>,
    /// FUSE requests arrive on dedicated blocking threads while the casita
    /// stores are async, so each request enters the caller's runtime here.
    /// Never a runtime worker thread, so this cannot re-enter the scheduler.
    runtime: tokio::runtime::Handle,
    inodes: RwLock<InodeTable>,
    /// One open blob reader per open file handle, so sequential reads of a
    /// large input do not re-open (or, for chunked stores, refetch) the blob
    /// on every 128 KiB request.
    files: RwLock<HashMap<u64, OpenFile>>,
    next_handle: AtomicU64,
    /// Shared with every other mount in this process by default: a
    /// listing is keyed by content, so one build decoding a stdenv
    /// directory spares every build after it.
    listings: Arc<Cache>,
    stats: MountStats,
    uid: u32,
    gid: u32,
}

///
/// Identical content shares an inode wherever it appears, which is what makes
/// the kernel cache a store path once rather than once per path that reaches
/// it. Inodes are never freed within a mount. Callers of shared input mounts
/// must retire mounts periodically to bound retention across builds.
#[derive(Default)]
struct InodeTable {
    by_key: HashMap<ContentKey, u64>,
    by_inode: HashMap<u64, FilesystemNode>,
    /// Only the mount root, whose entries are the roots this mount was
    /// given rather than a directory in the store: it has no content
    /// address to share with anyone else.
    root_listing: Option<Listing>,
    next: u64,
}

impl InodeTable {
    fn new() -> Self {
        Self {
            next: ROOT_ID + 1,
            ..Default::default()
        }
    }

    fn intern(&mut self, node: FilesystemNode) -> u64 {
        match self.by_key.entry(node.content_key()) {
            MapEntry::Occupied(existing) => *existing.get(),
            MapEntry::Vacant(slot) => {
                let inode = self.next;
                self.next += 1;
                slot.insert(inode);
                self.by_inode.insert(inode, node);
                inode
            }
        }
    }
}

impl StoreFs {
    /// Serve `view`, entering `runtime` for store access.
    ///
    /// `runtime` must outlive the mount; [`FuseMount`] holds the filesystem
    /// until it is unmounted.
    #[must_use]
    pub fn new(view: FilesystemView, runtime: tokio::runtime::Handle) -> Self {
        Self {
            view,
            input_registry: None,
            runtime,
            inodes: RwLock::new(InodeTable::new()),
            files: RwLock::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            listings: Cache::shared(),
            stats: MountStats::default(),
            // The mount is not `allow_other`: only the user who mounted it can
            // read it, and `default_permissions` checks these attributes.
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        }
    }

    /// Serve leased roots over one repository-scoped reader. Only the mount
    /// root changes; every input subtree remains immutable. Do not expose the
    /// complete mount to a sandbox: bind only names held by that build's lease.
    pub fn with_input_registry(
        content: Arc<dyn crate::ContentReader>,
        runtime: tokio::runtime::Handle,
        registry: Arc<InputRegistry>,
    ) -> Self {
        Self {
            input_registry: Some(registry),
            ..Self::new(FilesystemView::new(content, Default::default()), runtime)
        }
    }

    /// Serve `view` with a listing cache of your own, rather than the one
    /// this process shares. For tests that want to observe cold misses.
    #[must_use]
    pub fn with_listing_cache(
        view: FilesystemView,
        runtime: tokio::runtime::Handle,
        listings: Arc<ListingCache>,
    ) -> Self {
        Self {
            listings,
            ..Self::new(view, runtime)
        }
    }

    /// The node behind an inode, or `None` for the mount root, which is the
    /// listing of served roots rather than a node.
    fn node(&self, inode: u64) -> io::Result<Option<FilesystemNode>> {
        if inode == ROOT_ID {
            return Ok(None);
        }
        self.inodes
            .read()
            .map_err(|_| poisoned())?
            .by_inode
            .get(&inode)
            .cloned()
            .map(Some)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn directory(&self, inode: u64) -> io::Result<Option<FilesystemNode>> {
        match self.node(inode)? {
            None => Ok(None),
            Some(node) if node.kind() == FilesystemNodeKind::Directory => Ok(Some(node)),
            Some(_) => Err(io::Error::from_raw_os_error(libc::ENOTDIR)),
        }
    }

    fn intern(&self, node: FilesystemNode) -> io::Result<u64> {
        Ok(self.inodes.write().map_err(|_| poisoned())?.intern(node))
    }

    /// The listing for a directory inode, reading it from casita the first
    /// time and serving later lookups from memory. Content directories are
    /// immutable; a shared mount's leased root is refreshed on every request.
    fn listing(&self, inode: u64) -> io::Result<Listing> {
        let Some(directory) = self.directory(inode)? else {
            if let Some(registry) = &self.input_registry {
                return Ok(Arc::new(sorted(registry.roots()?)));
            }
            // The mount root is this mount's own root set, not a stored
            // directory, so it is cached here and nowhere else.
            if let Some(listing) = &self.inodes.read().map_err(|_| poisoned())?.root_listing {
                return Ok(Arc::clone(listing));
            }
            let listing: Listing = Arc::new(sorted(self.view.roots()));
            let mut inodes = self.inodes.write().map_err(|_| poisoned())?;
            return Ok(Arc::clone(inodes.root_listing.get_or_insert(listing)));
        };

        // Keyed by the directory's content address, so a hit is a hit no
        // matter which mount or which build decoded it first.
        let key = directory.content_key();
        if let Some(listing) = self.listings.get(&key) {
            return Ok(listing);
        }
        let listing: Listing = Arc::new(sorted(
            self.runtime
                .block_on(self.view.entries(&directory))
                .map_err(|error| store_failure("readdir", &error))?,
        ));
        self.listings.insert(key, Arc::clone(&listing));
        Ok(listing)
    }

    fn attr(&self, inode: u64, node: Option<&FilesystemNode>) -> Attr {
        let (mode, size) = match node {
            None => (libc::S_IFDIR | 0o555, 0),
            Some(node) => match node.kind() {
                FilesystemNodeKind::Directory => (libc::S_IFDIR | 0o555, 0),
                FilesystemNodeKind::Regular => (
                    libc::S_IFREG | if node.executable() { 0o555 } else { 0o444 },
                    node.size(),
                ),
                FilesystemNodeKind::Symlink => (libc::S_IFLNK | 0o777, node.size()),
            },
        };
        Attr {
            ino: inode,
            size,
            blocks: size.div_ceil(512),
            mode,
            // One link even for directories: `lndir` and friends read
            // `st_nlink` as the subdirectory count and skip the recursion when
            // it looks like a leaf, and a Merkle DAG has no cheap answer.
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            blksize: 4096,
            // Nix exposes store mtimes one second after the Unix epoch.
            atime: 1,
            mtime: 1,
            ctime: 1,
            ..Default::default()
        }
    }

    /// A cacheable "not here": inode zero with a lifetime, which the
    /// kernel keeps instead of asking again.
    fn negative_entry(&self) -> Entry {
        Entry {
            inode: 0,
            generation: 0,
            attr: Attr::default().into(),
            attr_flags: 0,
            attr_timeout: FOREVER,
            entry_timeout: FOREVER,
        }
    }

    fn entry(&self, inode: u64, node: Option<&FilesystemNode>) -> Entry {
        Entry {
            inode,
            generation: 0,
            attr: self.attr(inode, node).into(),
            attr_flags: 0,
            attr_timeout: FOREVER,
            entry_timeout: FOREVER,
        }
    }

    /// Hits and misses in the listing cache, which is shared across
    /// mounts: misses on a later mount mean directories no earlier build
    /// had touched.
    #[must_use]
    pub fn listing_cache_counts(&self) -> (u64, u64) {
        self.listings.counts()
    }

    /// A snapshot of what this mount has served so far.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    fn reader(&self, handle: u64) -> io::Result<OpenFile> {
        self.files
            .read()
            .map_err(|_| poisoned())?
            .get(&handle)
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))
    }
}

impl FileSystem for StoreFs {
    type Inode = u64;
    type Handle = u64;

    fn init(&self, _capable: FsOptions) -> io::Result<FsOptions> {
        Ok(FsOptions::ASYNC_READ
            // Serve attributes with the directory listing: a build walking a
            // store path would otherwise pay a lookup round trip per entry.
            | FsOptions::DO_READDIRPLUS
            | FsOptions::READDIRPLUS_AUTO
            | FsOptions::PARALLEL_DIROPS
            | FsOptions::CACHE_SYMLINKS)
    }

    fn lookup(&self, _ctx: &Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let _timer = self.stats.start(Op::Lookup);
        let name = name.to_bytes();
        if let Some(registry) = self.input_registry.as_ref().filter(|_| parent == ROOT_ID) {
            // Root lookups must reflect registration/removal immediately,
            // including a negative lookup made before a lease was issued.
            let mut entry = match registry.root(name)? {
                Some(node) => self.entry(self.intern(node.clone())?, Some(&node)),
                None => self.negative_entry(),
            };
            entry.entry_timeout = Duration::ZERO;
            return Ok(entry);
        }
        let listing = self.listing(parent)?;
        let Ok(index) = listing.binary_search_by(|entry| entry.name.as_slice().cmp(name)) else {
            // A negative entry the kernel is allowed to cache. A configure
            // script probes for hundreds of headers that are not there, in
            // every directory on its search path; answering ENOENT instead
            // sends every one of those probes back to us, from every
            // process it starts.
            return Ok(self.negative_entry());
        };
        let node = listing[index].node.clone();
        let inode = self.intern(node.clone())?;
        Ok(self.entry(inode, Some(&node)))
    }

    fn getattr(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        _handle: Option<Self::Handle>,
    ) -> io::Result<(stat64, Duration)> {
        let _timer = self.stats.start(Op::Getattr);
        let node = self.node(inode)?;
        Ok((self.attr(inode, node.as_ref()).into(), FOREVER))
    }

    fn readlink(&self, _ctx: &Context, inode: Self::Inode) -> io::Result<Vec<u8>> {
        let _timer = self.stats.start(Op::Readlink);
        match self.node(inode)? {
            Some(node) => node
                .symlink_target()
                .map(<[u8]>::to_vec)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL)),
            None => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        }
    }

    fn open(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        _fuse_flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions, Option<u32>)> {
        let _timer = self.stats.start(Op::Open);
        if flags as i32 & libc::O_ACCMODE != libc::O_RDONLY {
            return Err(io::Error::from_raw_os_error(libc::EROFS));
        }
        let node = self
            .node(inode)?
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EISDIR))?;
        if node.kind() != FilesystemNodeKind::Regular {
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        // No store access here: the blob opens on the first read.
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.files.write().map_err(|_| poisoned())?.insert(
            handle,
            Arc::new(Mutex::new(OpenBlob {
                node,
                reader: None,
                runtime: self.runtime.clone(),
            })),
        );
        // Nothing can change this content, so the kernel keeps whatever it
        // cached from an earlier open.
        Ok((Some(handle), OpenOptions::KEEP_CACHE, None))
    }

    fn read(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        w: &mut dyn ZeroCopyWriter,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        let _timer = self.stats.start(Op::Read);
        let file = self.reader(handle)?;
        let mut file = file.lock().map_err(|_| poisoned())?;
        if file.reader.is_none() {
            let opened = self
                .runtime
                .block_on(self.view.open(&file.node))
                .map_err(|error| store_failure("open", &error))?;
            file.reader = Some(opened);
        }
        let reader = file.reader.as_mut().expect("opened just above");
        let bytes = self.runtime.block_on(async {
            reader.seek(io::SeekFrom::Start(offset)).await?;
            // FUSE wants exactly `size` bytes short of EOF, so read until the
            // limit is filled instead of taking one buffer's worth.
            let mut bytes = Vec::with_capacity(size as usize);
            tokio::io::copy(&mut reader.as_mut().take(u64::from(size)), &mut bytes).await?;
            Ok::<_, io::Error>(bytes)
        })?;

        let wanted = bytes.len() as u64;
        // The writer may take the buffer in several pieces; a short write is a
        // truncated file, not a partial read.
        let written = io::copy(&mut Cursor::new(bytes), w)?;
        if written != wanted {
            warn!(written, wanted, "short write to the fuse reply buffer");
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.stats.add_bytes_read(written);
        Ok(written as usize)
    }

    fn release(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
        _flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        self.files.write().map_err(|_| poisoned())?.remove(&handle);
        Ok(())
    }

    fn opendir(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        // Nothing in this mount can change, so let the kernel keep the
        // directory it just read. Without this every `readdir` is a
        // round trip, and a build that walks a store path repeatedly
        // (every configure test, every header search) pays for the
        // listing every single time.
        self.directory(inode)?;
        if inode == ROOT_ID && self.input_registry.is_some() {
            return Ok((None, OpenOptions::empty()));
        }
        Ok((None, OpenOptions::CACHE_DIR))
    }

    fn readdir(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        _handle: Self::Handle,
        _size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let _timer = self.stats.start(Op::Readdir);
        for (position, entry) in self.listing(inode)?.iter().enumerate().skip(
            offset
                .try_into()
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
        ) {
            let child = self.intern(entry.node.clone())?;
            let accepted = add_entry(dir_entry(child, position, entry))?;
            // Zero means the reply buffer is full; the kernel asks again from
            // the offset of the last entry that fit.
            if accepted == 0 {
                break;
            }
        }
        Ok(())
    }

    fn readdirplus(
        &self,
        _ctx: &Context,
        inode: Self::Inode,
        _handle: Self::Handle,
        _size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry, Entry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let _timer = self.stats.start(Op::Readdir);
        for (position, entry) in self.listing(inode)?.iter().enumerate().skip(
            offset
                .try_into()
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
        ) {
            let child = self.intern(entry.node.clone())?;
            let mut attributes = self.entry(child, Some(&entry.node));
            if inode == ROOT_ID && self.input_registry.is_some() {
                attributes.entry_timeout = Duration::ZERO;
            }
            let accepted = add_entry(dir_entry(child, position, entry), attributes)?;
            if accepted == 0 {
                break;
            }
        }
        Ok(())
    }

    fn releasedir(
        &self,
        _ctx: &Context,
        _inode: Self::Inode,
        _flags: u32,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        Ok(())
    }

    fn access(&self, _ctx: &Context, inode: Self::Inode, mask: u32) -> io::Result<()> {
        // Resolve the inode so a stale one is still ENOENT, then answer from
        // the one fact this filesystem has: nothing here is writable.
        self.node(inode)?;
        if mask as i32 & libc::W_OK != 0 {
            return Err(io::Error::from_raw_os_error(libc::EROFS));
        }
        Ok(())
    }
}

/// Casita hands entries back in its own order; a lookup binary searches
/// them, so they are sorted once when the listing is built.
fn sorted(mut entries: Vec<FilesystemEntry>) -> Vec<FilesystemEntry> {
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn dir_entry<'a>(inode: u64, position: usize, entry: &'a FilesystemEntry) -> DirEntry<'a> {
    DirEntry {
        ino: inode,
        // Offsets are resume points: the kernel sends back the offset of the
        // last entry it accepted, so they count from one.
        offset: position as u64 + 1,
        type_: match entry.kind() {
            FilesystemNodeKind::Directory => libc::DT_DIR,
            FilesystemNodeKind::Regular => libc::DT_REG,
            FilesystemNodeKind::Symlink => libc::DT_LNK,
        }
        .into(),
        name: &entry.name,
    }
}

/// A store that cannot answer is an IO error to the build, never a missing
/// file: silently serving ENOENT would turn an infrastructure failure into a
/// wrong build result.
fn store_failure(operation: &str, error: &anyhow::Error) -> io::Error {
    warn!(operation, "{error:#}");
    io::Error::from_raw_os_error(libc::EIO)
}

fn poisoned() -> io::Error {
    io::Error::from_raw_os_error(libc::EIO)
}

#[cfg(test)]
mod tests;
