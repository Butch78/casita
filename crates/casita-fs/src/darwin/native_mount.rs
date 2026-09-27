use super::{native_protocol as protocol, setup};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

/// Native Rust FSKit mount of one local repository, without FUSE-T or TCP.
///
/// Run synchronous lifecycle methods off the async executor. Callers must keep
/// published objects durably reachable in the repository for the mount lifetime.
/// Independent processes can mount different repositories concurrently using the
/// shared installed extension. Run explicit setup before mounting. One native
/// mount per repository is supported.
pub struct NativeRepositoryMount {
    repository: PathBuf,
    session: tempfile::TempDir,
    directory: tempfile::TempDir,
    lock: Option<File>,
    mounted: bool,
    roots: HashMap<Vec<u8>, casita::Node>,
    installation: Option<fskit_native::setup::Installation>,
}

impl NativeRepositoryMount {
    pub fn new(repository: &Path, parent: &Path) -> io::Result<Self> {
        if unsafe { libc::geteuid() } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native FSKit mounts must run as an ordinary user",
            ));
        }
        let repository = repository.canonicalize()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(repository.join(protocol::LOCK))?;
        lock.try_lock().map_err(|e| {
            io::Error::other(format!(
                "repository already has a native mount controller: {e}"
            ))
        })?;
        // A stale record may still belong to a live extension after a controller
        // crash. Require explicit recovery rather than replacing its control files.
        if repository.join(protocol::ACTIVE).try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "native mount session already exists; unmount and inspect it before recovery",
            ));
        }
        let installation = setup::installed_native()?;
        fs::create_dir_all(parent)?;
        let directory = tempfile::Builder::new()
            .prefix(".casita-native-")
            .tempdir_in(parent.canonicalize()?)?;
        let session = tempfile::Builder::new()
            .prefix(".casita-native-session-")
            .tempdir_in(&repository)?;
        fs::write(session.path().join("version"), protocol::VERSION)?;
        let mut active = tempfile::NamedTempFile::new_in(&repository)?;
        active.write_all(session.path().file_name().unwrap().as_bytes())?;
        active.as_file().sync_all()?;
        active
            .persist_noclobber(repository.join(protocol::ACTIVE))
            .map_err(|e| e.error)?;
        let mut mount = Self {
            repository,
            session,
            directory,
            lock: Some(lock),
            mounted: false,
            roots: HashMap::new(),
            installation: Some(installation),
        };
        // Treat an interrupted command as potentially mounted until inspected.
        mount.mounted = true;
        // LaunchServices registration settles asynchronously. Retry only an
        // explicit helper-startup failure, never an ambiguous timeout or a
        // command that left a mounted volume. Preserve every other error.
        let discovery_deadline = Instant::now() + Duration::from_secs(15);
        let output = loop {
            mount.mounted = true;
            let output = run_bounded(
                Command::new("/sbin/mount")
                    .args(["-F", "-t", "casita"])
                    .arg(&mount.repository)
                    .arg(mount.path()),
            );
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    // A timed-out helper may have delegated work to FSKit. Retain
                    // the session even if the mount has not appeared yet.
                    mount.directory.disable_cleanup(true);
                    mount.session.disable_cleanup(true);
                    mount.mounted = false;
                    mount.lock.take();
                    return Err(error);
                }
            };
            mount.mounted = is_mounted(mount.path())?;
            if mount.mounted
                || !protocol::retryable_launch_failure(output.status.code(), &output.stderr)
                || Instant::now() >= discovery_deadline
            {
                break output;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        // Registration is not approval. Only macOS can authorize the mount, and
        // a successful helper exit alone is not proof that a volume was mounted.
        if !output.status.success() || !mount.mounted {
            return Err(io::Error::other(format!(
                "native FSKit mount failed ({}): {}{}\n\
                 If Casita is not enabled, open System Settings → General → \
                 Login Items & Extensions → By Category → File System Extensions, \
                 enable Casita, then retry. Use By Category: the By App toggle can \
                 fail even when approval through By Category works.",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(mount)
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    pub fn publish_root(&mut self, name: &[u8], node: casita::Node) -> io::Result<PathBuf> {
        if !self.mounted {
            return Err(io::Error::other("native mount is closed"));
        }
        let encoded = protocol::encode(name, node.clone())?;
        let path = self
            .path()
            .join("views")
            .join(std::ffi::OsStr::from_bytes(name));
        if let Some(previous) = self.roots.get(name) {
            if previous != &node {
                return Err(io::Error::other("immutable store path changed content"));
            }
            return Ok(path);
        }
        let staged = protocol::descriptor(self.session.path(), name);
        let mut file = tempfile::NamedTempFile::new_in(self.session.path())?;
        file.write_all(&encoded)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(&staged).map_err(|e| e.error)?;
        let result = match &node {
            casita::Node::Directory { .. } => fs::create_dir(&path),
            casita::Node::File { executable, .. } => OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CREAT | libc::O_EXCL)
                .mode(if *executable { 0o555 } else { 0o444 })
                .open(&path)
                .map(drop),
            casita::Node::Symlink { target } => {
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target.as_bytes()), &path)
            }
        };
        // Keep staged content on ambiguous VFS errors for diagnosis.
        result?;
        self.roots.insert(name.to_vec(), node);
        Ok(path)
    }

    /// Ordinary unmount. A busy volume remains mounted and can be retried.
    pub fn close(&mut self) -> io::Result<()> {
        if self.mounted && !is_mounted(self.path())? {
            self.mounted = false;
        }
        if self.mounted {
            let output = run_bounded(Command::new("/sbin/umount").arg(self.path()))?;
            if !output.status.success() || is_mounted(self.path())? {
                return Err(io::Error::other(format!(
                    "native FSKit unmount failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            self.mounted = false;
        }
        if self.lock.is_some() {
            fs::remove_file(self.repository.join(protocol::ACTIVE))?;
            self.lock.take();
        }
        self.installation.take();
        Ok(())
    }
}

fn run_bounded(command: &mut Command) -> io::Result<Output> {
    // Files avoid deadlocking on full stdout/stderr pipes while waiting.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native FSKit lifecycle command exceeded 30 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout)?;
    stderr.read_to_end(&mut output.stderr)?;
    Ok(output)
}

fn is_mounted(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.dev() != fs::metadata(path.parent().unwrap())?.dev())
}

impl Drop for NativeRepositoryMount {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            self.directory.disable_cleanup(true);
            self.session.disable_cleanup(true);
            tracing::error!(%error, path = %self.path().display(), "native FSKit cleanup failed; retained mount and control files");
        }
    }
}
