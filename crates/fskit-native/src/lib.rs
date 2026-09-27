//! Native Rust FSKit callbacks and platform-independent filesystem contracts.
//!
//! No Apple or application-specific types cross the backend boundary. Methods are synchronous
//! and may run concurrently on FSKit workers, never on an async executor.

use std::borrow::Cow;
use std::io;
use std::sync::{Arc, RwLock, RwLockReadGuard, TryLockError};

#[cfg(any(target_os = "macos", test))]
mod enumeration;
mod options;
pub use options::{Observation, VolumeOptions};
#[cfg(target_os = "macos")]
pub mod native;
#[cfg(all(unix, feature = "setup"))]
pub mod setup;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub id: u64,
    pub parent: u64,
    pub kind: FileKind,
    pub size: u64,
    /// POSIX permission bits, including executable permissions.
    pub mode: u16,
}

#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    pub name: Vec<u8>,
    pub id: u64,
    pub kind: FileKind,
    /// Allows enumeration with attributes without a second metadata lookup.
    pub metadata: Option<Metadata>,
}

/// Immutable enumeration snapshot. Cookies are indexes within this snapshot.
///
/// The transport must retain this object across pages and associate its own
/// verifier with it. A cookie must never be reused with a replacement snapshot.
/// Snapshots omit `.` and `..`; the transport handles those if necessary.
#[derive(Clone, Debug)]
pub struct DirectorySnapshot(pub Arc<[DirectoryEntry]>);

impl DirectorySnapshot {
    /// Visit entries until the sink is full. A rejected entry is not consumed.
    /// Returns the next cookie and whether enumeration reached the end.
    pub fn enumerate(
        &self,
        cookie: u64,
        mut sink: impl FnMut(&DirectoryEntry, u64) -> bool,
    ) -> io::Result<(u64, bool)> {
        let start = usize::try_from(cookie)
            .ok()
            .filter(|index| *index <= self.0.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid directory cookie")
            })?;
        for (index, entry) in self.0.iter().enumerate().skip(start) {
            if !sink(entry, index as u64 + 1) {
                return Ok((index as u64, false));
            }
        }
        Ok((self.0.len() as u64, true))
    }
}

pub trait Filesystem: Send + Sync {
    fn root(&self) -> u64;
    fn metadata(&self, id: u64) -> io::Result<Metadata>;
    fn lookup(&self, parent: u64, name: &[u8]) -> io::Result<Option<Metadata>>;
    fn directory(&self, id: u64) -> io::Result<DirectorySnapshot>;
    /// Initialize at most `output.len()` bytes, return that count, and leave the
    /// remainder unchanged. Return zero at EOF. Directories/symlinks are errors.
    fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> io::Result<usize>;
    /// Read initialized bytes for copying into a native buffer. Override to
    /// borrow immutable memory or preserve an existing allocating read path.
    fn read_data(&self, id: u64, offset: u64, length: usize) -> io::Result<Cow<'_, [u8]>> {
        let mut output = vec![0; length];
        let count = self.read(id, offset, &mut output)?;
        if count > output.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "read exceeded buffer",
            ));
        }
        output.truncate(count);
        Ok(Cow::Owned(output))
    }
    /// Return the complete, uninterpreted target bytes of a symbolic link.
    fn read_link(&self, id: u64) -> io::Result<Vec<u8>>;
    /// Release backend resources after all operations have finished. On error,
    /// the backend must remain usable and allow this operation to be retried.
    /// Successful shutdown is followed by dropping the backend.
    fn shutdown(&mut self) -> io::Result<()>;
    fn options(&self) -> VolumeOptions {
        VolumeOptions::default()
    }
    fn observe(&self, _event: Observation<'_>) {}
    /// Optional creation callback, including backend-specific control requests.
    /// Validation and publication policy belong to the backend.
    fn create(
        &self,
        _parent: u64,
        _name: &[u8],
        _kind: FileKind,
        _target: Option<&[u8]>,
    ) -> io::Result<Metadata> {
        Err(io::Error::new(
            io::ErrorKind::ReadOnlyFilesystem,
            "read-only filesystem",
        ))
    }
}

/// Backend-specific operations, such as publishing a pre-staged immutable root.
/// The FSKit adapter decides how a native callback maps onto these requests.
pub trait Control: Filesystem {
    type Request;
    type Reply;
    fn control(&self, request: Self::Request) -> io::Result<Self::Reply>;
}

/// Admission and retryable shutdown for a backend, independent of OS unmount.
///
/// The transport must confirm successful OS unmount separately. It must retain
/// this session after failed unmount or shutdown. All operations, including
/// controls, must enter through `with`; do not leak independent backend owners.
/// Dropping this value is ordinary Rust cleanup, not a successful shutdown.
pub struct BackendSession<B> {
    backend: RwLock<Option<B>>,
}

impl<B: Filesystem> BackendSession<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend: RwLock::new(Some(backend)),
        }
    }

    pub fn with<T>(&self, operation: impl FnOnce(&B) -> io::Result<T>) -> io::Result<T> {
        operation(&*self.acquire()?)
    }

    /// Retain admission through a native reply and its observations.
    pub fn acquire(&self) -> io::Result<BackendAccess<'_, B>> {
        let guard = self
            .backend
            .read()
            .map_err(|_| io::Error::other("backend lock poisoned"))?;
        let _backend = guard
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "backend closed"))?;
        Ok(BackendAccess(guard))
    }

    /// Nonblocking admission check. An active operation returns `WouldBlock`.
    /// A failed shutdown retains the backend; a successful close is idempotent.
    pub fn close(&self) -> io::Result<()> {
        let mut guard = match self.backend.try_write() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "backend operations still active",
                ));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("backend lock poisoned"));
            }
        };
        if let Some(backend) = guard.as_mut() {
            backend.shutdown()?;
        }
        drop(guard.take());
        Ok(())
    }
}

pub struct BackendAccess<'a, B>(RwLockReadGuard<'a, Option<B>>);
impl<B> std::ops::Deref for BackendAccess<'_, B> {
    type Target = B;
    fn deref(&self) -> &B {
        self.0.as_ref().expect("admission checked under read lock")
    }
}

impl<T: Filesystem + ?Sized> Filesystem for Box<T> {
    fn root(&self) -> u64 {
        (**self).root()
    }
    fn metadata(&self, id: u64) -> io::Result<Metadata> {
        (**self).metadata(id)
    }
    fn lookup(&self, parent: u64, name: &[u8]) -> io::Result<Option<Metadata>> {
        (**self).lookup(parent, name)
    }
    fn directory(&self, id: u64) -> io::Result<DirectorySnapshot> {
        (**self).directory(id)
    }
    fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        (**self).read(id, offset, output)
    }
    fn read_data(&self, id: u64, offset: u64, length: usize) -> io::Result<Cow<'_, [u8]>> {
        (**self).read_data(id, offset, length)
    }
    fn read_link(&self, id: u64) -> io::Result<Vec<u8>> {
        (**self).read_link(id)
    }
    fn shutdown(&mut self) -> io::Result<()> {
        (**self).shutdown()
    }
    fn options(&self) -> VolumeOptions {
        (**self).options()
    }
    fn observe(&self, event: Observation<'_>) {
        (**self).observe(event)
    }
    fn create(
        &self,
        parent: u64,
        name: &[u8],
        kind: FileKind,
        target: Option<&[u8]>,
    ) -> io::Result<Metadata> {
        (**self).create(parent, name, kind, target)
    }
}
