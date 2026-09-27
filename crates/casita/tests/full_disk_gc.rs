#![cfg(feature = "experimental")]

//! Linux filesystem-level proof that local collection recovers from ENOSPC.

#![cfg(all(target_os = "linux", feature = "native"))]

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use casita::experimental::{
    BlobStore as _, ClosureStatus, Directory, MetadataError, MetadataStore as _, Repository,
    RepositoryError, RootName, SpillLimits,
};

const CHILD_ENV: &str = "CASITA_FULL_DISK_GC_CHILD";
const TEST_NAME: &str = "local_gc_succeeds_after_filesystem_returns_enospc";
const INSUFFICIENT_CHILD_ENV: &str = "CASITA_INSUFFICIENT_GC_CHILD";
const INSUFFICIENT_TEST_NAME: &str = "local_gc_preserves_roots_when_garbage_cannot_release_space";
const SPILL_CHILD_ENV: &str = "CASITA_SPILL_GC_CHILD";
const SPILL_TEST_NAME: &str = "local_gc_fails_cleanly_when_traversal_state_cannot_spill";

/// Whether this machine lets an unprivileged process create the user and mount
/// namespaces these tests need.
///
/// Mounting a private tmpfs is the only way to reach a genuinely full
/// filesystem without touching the host, and it needs `CAP_SYS_ADMIN` inside a
/// fresh namespace. Some kernels refuse that to unprivileged processes
/// (`kernel.apparmor_restrict_unprivileged_userns=1`, `user.max_user_namespaces=0`,
/// or a container profile blocking the syscall). Refusing is an answer about
/// the machine, not about casita, so these tests report it and stop rather
/// than failing a build for it.
fn mount_namespaces_available() -> bool {
    match Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "true"])
        .output()
    {
        Ok(output) => output.status.success(),
        // No util-linux at all.
        Err(_) => false,
    }
}

/// Report a skipped filesystem-level test so it cannot be mistaken for one
/// that ran.
fn skip_without_namespaces(test: &str) -> bool {
    if mount_namespaces_available() {
        return false;
    }
    eprintln!(
        "SKIP {test}: this machine does not permit unprivileged user and mount \
         namespaces, so a full filesystem cannot be created to test against"
    );
    true
}

#[test]
fn local_gc_succeeds_after_filesystem_returns_enospc() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_in_mount_namespace();
        return;
    }

    if skip_without_namespaces(TEST_NAME) {
        return;
    }

    // Mounting a private tmpfs needs CAP_SYS_ADMIN. An unprivileged user
    // namespace grants that capability only inside a fresh mount namespace,
    // without changing the host or requiring a privileged CI runner.
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, "1")
        .output()
        .expect("launch the test inside an unprivileged mount namespace");
    assert!(
        output.status.success(),
        "full-disk child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn local_gc_fails_cleanly_when_traversal_state_cannot_spill() {
    if std::env::var_os(SPILL_CHILD_ENV).is_some() {
        let filesystem = MountedTmpfs::new(32 * 1024 * 1024);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(assert_spill_failure_preserves_the_repository(
            filesystem.path(),
        ));
        return;
    }

    if skip_without_namespaces(SPILL_TEST_NAME) {
        return;
    }

    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", SPILL_TEST_NAME, "--nocapture"])
        .env(SPILL_CHILD_ENV, "1")
        .output()
        .expect("launch the test inside an unprivileged mount namespace");
    assert!(
        output.status.success(),
        "full-disk spill child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn local_gc_preserves_roots_when_garbage_cannot_release_space() {
    if std::env::var_os(INSUFFICIENT_CHILD_ENV).is_some() {
        let filesystem = MountedTmpfs::new(32 * 1024 * 1024);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(assert_insufficient_reclaimable_space(filesystem.path()));
        return;
    }

    if skip_without_namespaces(INSUFFICIENT_TEST_NAME) {
        return;
    }

    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", INSUFFICIENT_TEST_NAME, "--nocapture"])
        .env(INSUFFICIENT_CHILD_ENV, "1")
        .output()
        .expect("launch the insufficient-space test in an unprivileged mount namespace");
    assert!(
        output.status.success(),
        "insufficient-space child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn run_in_mount_namespace() {
    let filesystem = MountedTmpfs::new(64 * 1024 * 1024);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(assert_full_disk_collection(filesystem.path()));
}

async fn assert_full_disk_collection(filesystem: &Path) {
    let repository_root = filesystem.join("repository");
    let repository = Repository::local(&repository_root).await.unwrap();
    assert!(
        !repository_root.join("gc.reserve").exists(),
        "the default profile must not hide preallocated collection space"
    );
    let mutation = repository.mutation_session().await.unwrap();

    let rooted = mutation
        .stage_blob(b"rooted data must survive")
        .await
        .unwrap();
    let rooted_key = rooted.record().key().clone();
    let rooted_payload = rooted.record().payload();
    mutation
        .publish_rooted(
            vec![rooted],
            RootName::try_from("full-disk/live").unwrap(),
            rooted_key.clone(),
        )
        .await
        .unwrap();

    // Incompressible data ensures the emergency physical sweep releases
    // substantially more than the state transaction needs to commit.
    let stale_bytes = deterministic_bytes(12 * 1024 * 1024);
    let stale = mutation.stage_blob(&stale_bytes).await.unwrap();
    let stale_key = stale.record().key().clone();
    let stale_payload = stale.record().payload();
    mutation.publish_unrooted(vec![stale]).await.unwrap();
    drop(mutation);
    drop(repository);

    // Create the probe inode before exhaustion, then consume every remaining
    // data block. The asserted errno is the precondition that distinguishes
    // this test from a mocked capacity observation.
    let probe_path = filesystem.join("enospc-probe");
    File::create(&probe_path).unwrap();
    let mut filler = File::create(filesystem.join("filler")).unwrap();
    let block = vec![0xa5; 1024 * 1024];
    let fill_error = loop {
        match filler.write(&block) {
            Ok(0) => panic!("filler made no progress before ENOSPC"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert_enospc(&fill_error);
    assert_eq!(
        free_space(filesystem),
        0,
        "the test filesystem must be full"
    );

    let mut probe = OpenOptions::new().write(true).open(&probe_path).unwrap();
    let probe_error = probe.write_all(&[0x5a; 4096]).unwrap_err();
    assert_enospc(&probe_error);
    drop(probe);

    // Exercise the ordinary mutation-start recovery path: no database or lock
    // handle remains open from before exhaustion, and opening the session at
    // zero free bytes must collect before taking its shared lease.
    let repository = Repository::local(&repository_root).await.unwrap();
    let mutation = repository.mutation_session().await.unwrap();
    drop(mutation);

    let snapshot = repository.metadata().snapshot().await.unwrap();
    assert!(snapshot.object(&stale_key).await.unwrap().is_none());
    assert!(!repository.payloads().has(&stale_payload).await.unwrap());
    assert!(repository.payloads().has(&rooted_payload).await.unwrap());
    assert!(matches!(
        repository.verify_closure(&rooted_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    drop(snapshot);

    // The automatic pass completed this handle's once-only attempt. An
    // explicit pass now has no work left.
    let after_automatic = repository.collect().await.unwrap();
    assert_eq!(after_automatic.revision, None);
    assert_eq!(after_automatic.removed, Default::default());

    // A second pass with no logical garbage must not manufacture a state
    // write. Fill the reclaimed blocks again and prove the zero-byte no-op.
    let refill_error = loop {
        match filler.write(&block) {
            Ok(0) => panic!("filler made no progress before the second ENOSPC"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert_enospc(&refill_error);
    assert_eq!(free_space(filesystem), 0);
    let noop = repository.collect().await.unwrap();
    assert_eq!(noop.revision, None);
    assert_eq!(noop.removed, Default::default());
    drop(filler);
}

async fn assert_insufficient_reclaimable_space(filesystem: &Path) {
    let repository_root = filesystem.join("repository");
    let repository = Repository::local(&repository_root).await.unwrap();
    let mutation = repository.mutation_session().await.unwrap();

    // These are distinct logical records over exactly the same physical
    // payload. The directory is rooted and the blob record is unreachable, so
    // GC has logical work but no physical block it is allowed to reclaim.
    let directory = Directory::new();
    let encoded = directory.encode();
    let rooted = mutation.stage_directory(&directory).await.unwrap();
    let rooted_key = rooted.record().key().clone();
    let shared_payload = rooted.record().payload();
    let orphan = mutation.stage_blob(&encoded).await.unwrap();
    let orphan_key = orphan.record().key().clone();
    assert_eq!(orphan.record().payload(), shared_payload);
    mutation
        .publish_rooted(
            vec![rooted, orphan],
            RootName::try_from("full-disk/shared-payload").unwrap(),
            rooted_key.clone(),
        )
        .await
        .unwrap();
    drop(mutation);
    drop(repository);

    let probe_path = filesystem.join("enospc-probe");
    File::create(&probe_path).unwrap();
    let mut filler = File::create(filesystem.join("filler")).unwrap();
    let block = vec![0x3c; 1024 * 1024];
    let fill_error = loop {
        match filler.write(&block) {
            Ok(0) => panic!("filler made no progress before ENOSPC"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert_enospc(&fill_error);
    assert_eq!(free_space(filesystem), 0);
    let mut probe = OpenOptions::new().write(true).open(&probe_path).unwrap();
    assert_enospc(&probe.write_all(&[0x5a; 4096]).unwrap_err());
    drop(probe);

    let repository = Repository::local(&repository_root).await.unwrap();
    assert!(matches!(
        repository.collect().await,
        Err(RepositoryError::Metadata(MetadataError::StorageFull))
    ));
    let snapshot = repository.metadata().snapshot().await.unwrap();
    assert!(snapshot.object(&rooted_key).await.unwrap().is_some());
    assert!(snapshot.object(&orphan_key).await.unwrap().is_some());
    drop(snapshot);
    assert!(repository.payloads().has(&shared_payload).await.unwrap());
    // No process is collecting now; verify the rooted bytes directly while
    // the interrupted prune fence still excludes online read admission.
    assert_eq!(
        repository
            .payloads()
            .read_to_vec(&shared_payload)
            .await
            .unwrap(),
        Some(encoded)
    );
    assert!(matches!(
        repository.fsck().await,
        Err(RepositoryError::Busy(_))
    ));
    filler.set_len(0).unwrap();
    drop(filler);
    repository.collect().await.unwrap();
    assert!(matches!(
        repository.verify_closure(&rooted_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    assert!(repository.fsck().await.unwrap().is_healthy());
}

/// A collection whose traversal must spill cannot write its temporary state on
/// a full filesystem. It has to fail, and leave the repository exactly as it
/// found it.
async fn assert_spill_failure_preserves_the_repository(filesystem: &Path) {
    let repository_root = filesystem.join("repository");
    let source = filesystem.join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    for entry in 0..8 {
        std::fs::write(
            source.join("nested").join(format!("file-{entry}")),
            format!("spill me {entry}").as_bytes(),
        )
        .unwrap();
    }

    // One object in memory: every traversal past the root has to spill.
    let spilling = SpillLimits {
        max_memory_objects: 1,
        max_spill_bytes: 64 * 1024 * 1024,
    };
    let name = RootName::try_from("full-disk/spilling").unwrap();
    let repository = Repository::local(&repository_root)
        .await
        .unwrap()
        .with_spill_limits(spilling);
    let root = repository
        .import(casita::import::FilesystemImport::new(&source, name.clone()))
        .await
        .unwrap();
    drop(repository);

    let mut filler = File::create(filesystem.join("filler")).unwrap();
    let block = vec![0x3c; 1024 * 1024];
    let fill_error = loop {
        match filler.write(&block) {
            Ok(0) => panic!("filler made no progress before ENOSPC"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert_enospc(&fill_error);
    assert_eq!(free_space(filesystem), 0);

    let repository = Repository::local(&repository_root)
        .await
        .unwrap()
        .with_spill_limits(spilling);
    assert!(
        repository.collect().await.is_err(),
        "collection cannot spill onto a full filesystem"
    );

    // Space back: the root, its closure, and the repository as a whole are
    // untouched, and no spill file survived the failure.
    drop(filler);
    std::fs::remove_file(filesystem.join("filler")).ok();
    let snapshot = repository.metadata().snapshot().await.unwrap();
    assert_eq!(snapshot.root(&name).await.unwrap(), Some(root.clone()));
    drop(snapshot);
    assert!(matches!(
        repository.verify_closure(&root).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    assert!(repository.fsck().await.unwrap().is_healthy());
    let spilled: Vec<_> = std::fs::read_dir(repository_root.join("spill"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(spilled.is_empty(), "a failed spill leaked {spilled:?}");
}

fn assert_enospc(error: &io::Error) {
    assert_eq!(
        error.raw_os_error(),
        Some(libc::ENOSPC),
        "expected ENOSPC, got {error:?}"
    );
}

fn free_space(path: &Path) -> u64 {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    assert_eq!(result, 0, "statvfs: {}", io::Error::last_os_error());
    let stats = unsafe { stats.assume_init() };
    stats.f_bavail.saturating_mul(stats.f_frsize)
}

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

struct MountedTmpfs {
    directory: tempfile::TempDir,
    path: PathBuf,
}

impl MountedTmpfs {
    fn new(bytes: u64) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let target = CString::new(path.as_os_str().as_bytes()).unwrap();
        let filesystem = c"tmpfs";
        let options = CString::new(format!("size={bytes},nr_inodes=16384")).unwrap();
        let result = unsafe {
            libc::mount(
                filesystem.as_ptr(),
                target.as_ptr(),
                filesystem.as_ptr(),
                0,
                options.as_ptr().cast(),
            )
        };
        assert_eq!(result, 0, "mount tmpfs: {}", io::Error::last_os_error());
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MountedTmpfs {
    fn drop(&mut self) {
        let target = CString::new(self.path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
        if result != 0 && !std::thread::panicking() {
            panic!("unmount tmpfs: {}", io::Error::last_os_error());
        }
        let _ = &self.directory;
    }
}
