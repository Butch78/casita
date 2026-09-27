use crate::filesystem::*;
use crate::MemoryFilesystem;
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

fn check_read(fs: &dyn Filesystem, parent: u64, name: &[u8], expected: &[u8]) {
    let file = fs.lookup(parent, name).unwrap().unwrap();
    assert_eq!(file.kind, FileKind::File);
    assert_eq!(fs.metadata(file.id).unwrap(), file);
    for offset in [0, 1, expected.len() as u64, u64::MAX] {
        let mut output = [0xaa; 32];
        let count = fs.read(file.id, offset, &mut output).unwrap();
        let tail = usize::try_from(offset)
            .ok()
            .and_then(|index| expected.get(index..))
            .unwrap_or_default();
        let wanted = tail.len().min(output.len());
        assert_eq!(count, wanted);
        assert_eq!(&output[..count], &tail[..wanted]);
        assert_eq!(
            fs.read_data(file.id, offset, output.len())
                .unwrap()
                .as_ref(),
            &tail[..wanted]
        );
        assert!(output[count..].iter().all(|byte| *byte == 0xaa));
    }
    assert_eq!(fs.read(file.id, 0, &mut []).unwrap(), 0);
    assert_eq!(
        fs.directory(file.id).unwrap_err().kind(),
        io::ErrorKind::NotADirectory
    );
    assert!(fs.lookup(parent, b"absent").unwrap().is_none());
    assert!(fs.read(parent, 0, &mut [0; 1]).is_err());
}

fn check_directory(fs: &dyn Filesystem, parent: u64) {
    let snapshot = fs.directory(parent).unwrap();
    let mut cookie = 0;
    let mut names = Vec::new();
    loop {
        let mut admitted = false;
        let (next, eof) = snapshot
            .enumerate(cookie, |entry, next| {
                if admitted {
                    return false;
                }
                admitted = true;
                assert_eq!(next, cookie + 1);
                let found = fs.lookup(parent, &entry.name).unwrap().unwrap();
                assert_eq!(found.id, entry.id);
                assert_eq!(entry.metadata, Some(found));
                names.push(entry.name.clone());
                true
            })
            .unwrap();
        cookie = next;
        if eof {
            break;
        }
        assert!(admitted);
    }
    assert_eq!(names.len(), snapshot.0.len());
    assert_eq!(
        snapshot.enumerate(0, |_, _| false).unwrap(),
        (0, snapshot.0.is_empty())
    );
    assert!(snapshot.enumerate(u64::MAX, |_, _| true).is_err());
}

#[test]
fn memory_contract_preserves_bytes_metadata_and_pagination() {
    let fs = MemoryFilesystem::default();
    check_directory(&fs, fs.root());
    check_read(&fs, fs.root(), b"run", crate::SCRIPT);
    let executable = Filesystem::lookup(&fs, fs.root(), b"run").unwrap().unwrap();
    assert_ne!(executable.mode & 0o111, 0);
    let link = Filesystem::lookup(&fs, fs.root(), b"link")
        .unwrap()
        .unwrap();
    assert_eq!(fs.read_link(link.id).unwrap(), b"size-4096");
    assert!(Filesystem::read(&fs, link.id, 0, &mut [0; 2]).is_err());
    #[cfg(not(feature = "portable-names"))]
    check_read(&fs, fs.root(), b"byte-\xff", b"byte-safe\n");
}

#[test]
fn close_rejects_active_calls_and_stops_admission_after_success() {
    let session = Arc::new(BackendSession::new(MemoryFilesystem::default()));
    let worker_session = session.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_session
            .with(|fs| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                fs.metadata(fs.root())
            })
            .unwrap()
    });
    entered_rx.recv().unwrap();
    assert_eq!(
        session.close().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    // Shared admission permits concurrent reads while the first is active.
    session.with(|fs| fs.metadata(fs.root())).unwrap();
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    session.close().unwrap();
    session.close().unwrap();
    assert_eq!(
        session.with(|_| Ok(())).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
}

struct RetryBackend {
    memory: MemoryFilesystem,
    fail: bool,
    dropped: Arc<AtomicBool>,
}
impl Drop for RetryBackend {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
impl Filesystem for RetryBackend {
    fn root(&self) -> u64 {
        self.memory.root()
    }
    fn metadata(&self, id: u64) -> io::Result<Metadata> {
        self.memory.metadata(id)
    }
    fn lookup(&self, parent: u64, name: &[u8]) -> io::Result<Option<Metadata>> {
        Filesystem::lookup(&self.memory, parent, name)
    }
    fn directory(&self, id: u64) -> io::Result<DirectorySnapshot> {
        self.memory.directory(id)
    }
    fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        Filesystem::read(&self.memory, id, offset, output)
    }
    fn read_link(&self, id: u64) -> io::Result<Vec<u8>> {
        self.memory.read_link(id)
    }
    fn shutdown(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.fail) {
            Err(io::Error::other("injected shutdown failure"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_shutdown_retains_usable_backend_for_retry() {
    let dropped = Arc::new(AtomicBool::new(false));
    let session = BackendSession::new(RetryBackend {
        memory: MemoryFilesystem::default(),
        fail: true,
        dropped: dropped.clone(),
    });
    assert!(session.close().is_err());
    assert!(!dropped.load(Ordering::SeqCst));
    session
        .with(|fs| {
            check_read(fs, fs.root(), b"run", crate::SCRIPT);
            Ok(())
        })
        .unwrap();
    session.close().unwrap();
    assert!(dropped.load(Ordering::SeqCst));
}

#[cfg(all(feature = "repository", unix))]
#[test]
fn repository_uses_the_same_contract_and_keeps_publication_separate() -> anyhow::Result<()> {
    use crate::{
        repository::{import, Backend},
        PublishRoot,
    };
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    };
    let base = tempfile::tempdir()?;
    let source = base.path().join("source");
    let repository = base.path().join("repository");
    std::fs::create_dir(&source)?;
    // APFS cannot materialize an invalid UTF-8 source name. The memory contract
    // and production mount smoke cover byte names without that host limitation.
    let raw_name: &[u8] = if cfg!(target_os = "macos") {
        b"data"
    } else {
        b"data-\xff"
    };
    let path = source.join(std::ffi::OsStr::from_bytes(raw_name));
    std::fs::write(&path, b"repository bytes")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    symlink(std::ffi::OsStr::from_bytes(raw_name), source.join("link"))?;
    import(&source, &repository, "fixture", "snapshot.json")?;
    let session = BackendSession::new(Backend::open(&repository)?);
    session.with(|fs| {
        let views = Filesystem::lookup(fs, fs.root(), b"views")?.unwrap();
        let fixture = Filesystem::lookup(fs, views.id, b"fixture")?.unwrap();
        check_directory(fs, fixture.id);
        check_read(fs, fixture.id, raw_name, b"repository bytes");
        assert_ne!(
            Filesystem::lookup(fs, fixture.id, raw_name)?.unwrap().mode & 0o111,
            0
        );
        let link = Filesystem::lookup(fs, fixture.id, b"link")?.unwrap();
        assert_eq!(fs.read_link(link.id)?, raw_name);
        let before = Filesystem::directory(fs, views.id)?;
        std::fs::copy(
            repository.join("snapshot.json"),
            repository.join("stage-later.json"),
        )?;
        let published = fs.control(PublishRoot {
            name: b"later".to_vec(),
            kind: FileKind::Directory,
            target: None,
        })?;
        assert_eq!(published.kind, FileKind::Directory);
        assert_eq!(before.0.len(), 1);
        assert_eq!(Filesystem::directory(fs, views.id)?.0.len(), 2);
        assert!(fs
            .control(PublishRoot {
                name: b"later".to_vec(),
                kind: FileKind::Directory,
                target: None
            })
            .is_err());
        Ok(())
    })?;
    session.close()?;
    // Successful shutdown drops repository owners before reopening.
    let mut reopened = Backend::open(&repository)?;
    reopened.shutdown()?;
    Ok(())
}
