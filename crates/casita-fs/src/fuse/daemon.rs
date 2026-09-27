//! Mounting a [`StoreFs`] on `/dev/fuse`.
//!
//! One session, one server thread per channel. The threads are plain blocking
//! threads on purpose: a FUSE request is answered synchronously, and each one
//! enters the caller's tokio runtime for the store access it needs.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::transport::{FuseChannel, FuseSession};
use tracing::{debug, info, warn};

use super::{StatsSnapshot, StoreFs};

/// A mounted [`StoreFs`]. Unmounts on drop.
pub struct FuseMount {
    /// Taken by the first unmount, so an explicit unmount and the drop guard
    /// cannot both tear the session down.
    session: Mutex<Option<FuseSession>>,
    /// Kept beside the server so the mount can report what it served.
    filesystem: Arc<StoreFs>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    mountpoint: PathBuf,
}

impl FuseMount {
    /// Mount `fs` at `mountpoint`, which must already be a directory owned by
    /// this user.
    ///
    /// Mounting needs `CAP_SYS_ADMIN` or the `fusermount3` helper; the backend
    /// falls back to the helper when the direct mount is refused, so this
    /// works as an ordinary user wherever FUSE is allowed at all.
    ///
    /// `threads` server threads answer requests concurrently. Reads block on
    /// the store, so this bounds how many store fetches a build can have in
    /// flight, not how much CPU the mount uses.
    pub fn new(fs: StoreFs, mountpoint: &Path, threads: usize) -> io::Result<Self> {
        let threads = threads.max(1);
        let fs = Arc::new(fs);
        let server = Arc::new(Server::new(Arc::clone(&fs)));
        let mut session = FuseSession::new(mountpoint, "casita-store", "", true)
            .map_err(|error| io::Error::other(format!("fuse session: {error}")))?;
        // The mount is for this user only: a build in a bubblewrap namespace
        // keeps its uid, so it can read the mount without opening it to
        // everyone else on the machine.
        session.set_allow_other(false);
        session
            .mount()
            .map_err(|error| io::Error::other(format!("fuse mount {mountpoint:?}: {error}")))?;

        let mount = Self {
            session: Mutex::new(Some(session)),
            filesystem: fs,
            threads: Mutex::new(Vec::with_capacity(threads)),
            mountpoint: mountpoint.to_owned(),
        };
        // Anything that fails from here on leaves a mounted filesystem, so
        // hand ownership to `mount` first and let its drop guard unmount.
        for index in 0..threads {
            let channel = {
                let mut session = mount.session.lock().map_err(|_| poisoned())?;
                let session = session.as_mut().expect("session is present until unmount");
                session
                    .new_channel()
                    .map_err(|error| io::Error::other(format!("fuse channel: {error}")))?
            };
            let server = Arc::clone(&server);
            let thread = std::thread::Builder::new()
                .name(format!("casita-fuse-{index}"))
                .spawn(move || serve(&server, channel))?;
            mount.threads.lock().map_err(|_| poisoned())?.push(thread);
        }
        debug!(mountpoint = ?mount.mountpoint, threads, "mounted store");
        Ok(mount)
    }

    /// What this mount has served so far: call counts, time inside
    /// each handler, and bytes. Read it after the build to see whether
    /// a slow build paid for round trips or for bytes.
    #[must_use]
    pub fn stats(&self) -> StatsSnapshot {
        self.filesystem.stats()
    }

    #[must_use]
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// Unmount and wait for the server threads. Idempotent.
    pub fn unmount(&self) -> io::Result<()> {
        let session = self.session.lock().map_err(|_| poisoned())?.take();
        let Some(mut session) = session else {
            return Ok(());
        };
        let result = session.umount().map_err(|error| {
            io::Error::other(format!("fuse umount {:?}: {error}", self.mountpoint))
        });
        // Unmounting closes the device, which is what ends the server loops.
        // Wake the channels as well so a thread that missed it still exits,
        // then wait: the threads borrow the filesystem, and the caller is
        // about to delete the directory underneath them.
        if let Err(error) = session.wake() {
            debug!(%error, "waking fuse channels");
        }
        for thread in self.threads.lock().map_err(|_| poisoned())?.drain(..) {
            let _ = thread.join();
        }
        let (hits, misses) = self.filesystem.listing_cache_counts();
        info!(
            mountpoint = ?self.mountpoint,
            stats = %self.filesystem.stats(),
            listing_hits = hits,
            listing_misses = misses,
            "unmounted store"
        );
        result
    }
}

impl Drop for FuseMount {
    fn drop(&mut self) {
        if let Err(error) = self.unmount() {
            warn!(%error, "failed to unmount store filesystem");
        }
    }
}

/// Answer requests until the session ends.
fn serve(server: &Server<Arc<StoreFs>>, mut channel: FuseChannel) {
    loop {
        match channel.get_request() {
            // The session is shutting down, or the filesystem was unmounted.
            Ok(None) => break,
            Ok(Some((reader, writer))) => {
                if let Err(error) = server.handle_message(reader, writer.into(), None, None) {
                    match error {
                        // The kernel side is gone; nothing left to reply to.
                        fuse_backend_rs::Error::EncodeMessage(error)
                            if error.raw_os_error() == Some(libc::EBADFD) =>
                        {
                            break
                        }
                        error => warn!(?error, "failed to handle a fuse request"),
                    }
                }
            }
            Err(error) => {
                debug!(%error, "fuse channel closed");
                break;
            }
        }
    }
}

fn poisoned() -> io::Error {
    io::Error::other("fuse mount state poisoned")
}
