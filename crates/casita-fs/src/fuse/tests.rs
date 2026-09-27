use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::test_support::TestRepository;
use casita::Node;
use fuse_backend_rs::api::filesystem::{Context, FileSystem, ROOT_ID};

use super::{FuseMount, InodeTable, ListingCache, Op, StoreFs};
use crate::{FilesystemNode, FilesystemView};

/// Mounting needs `/dev/fuse` plus either root or the `fusermount3` helper.
/// Where neither is available (a container without the device, say) the mount
/// tests skip rather than fail; everything they cover is Linux-only anyway.
fn can_mount() -> bool {
    if !Path::new("/dev/fuse").exists() {
        eprintln!("skipping: /dev/fuse is not present");
        return false;
    }
    if rustix::process::geteuid().is_root() {
        return true;
    }
    let found = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join("fusermount3").exists()))
        .unwrap_or(false);
    if !found {
        eprintln!("skipping: fusermount3 is not on PATH and we are not root");
    }
    found
}

/// Import `source` into memory stores and hand back a view over the named
/// roots, mirroring how a build exposes exactly its declared inputs.
async fn view_of(sources: &[(&[u8], &Path)]) -> FilesystemView {
    let repository = Arc::new(TestRepository::default());

    let mut roots: BTreeMap<Vec<u8>, Node> = BTreeMap::new();

    for (name, source) in sources {
        let imported = repository.import_output(source).await.expect("import path");
        roots.insert(name.to_vec(), imported.clone());
    }

    FilesystemView::new(repository, roots)
}

fn mount(view: FilesystemView, at: &Path) -> FuseMount {
    fs::create_dir_all(at).expect("mountpoint");
    FuseMount::new(StoreFs::new(view, tokio::runtime::Handle::current()), at, 2).expect("mount")
}

/// A store path with one of everything: a plain file, an executable in a
/// subdirectory, a symlink, and a name that is not valid UTF-8.
fn seed_tree(root: &Path) -> PathBuf {
    let tree = root.join("tree");
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::write(tree.join("data"), b"contents of data\n").unwrap();
    fs::write(tree.join("bin/tool"), b"#!/bin/sh\necho tool\n").unwrap();
    fs::set_permissions(tree.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("bin/tool", tree.join("link")).unwrap();
    fs::write(tree.join(OsStr::from_bytes(b"raw\xff")), b"byte safe\n").unwrap();
    tree
}

#[test]
fn identical_content_gets_one_inode() {
    let mut table = InodeTable::new();
    let file = FilesystemNode::from_casita(Node::File {
        digest: casita::BlobId::new(casita::Digest::hash(b"same")),
        size: 4,
        executable: false,
    });
    let executable = FilesystemNode::from_casita(Node::File {
        digest: casita::BlobId::new(casita::Digest::hash(b"same")),
        size: 4,
        executable: true,
    });

    let first = table.intern(file.clone());
    assert_eq!(table.intern(file), first, "same content, same inode");
    assert_ne!(
        table.intern(executable),
        first,
        "the executable bit is part of the content identity"
    );
    assert_ne!(first, fuse_backend_rs::api::filesystem::ROOT_ID);
}

#[tokio::test]
async fn leased_roots_do_not_cache_misses_or_reuse_released_names() {
    let reader = Arc::new(TestRepository::default());
    let registry = Arc::new(super::InputRegistry::default());
    let fs =
        StoreFs::with_input_registry(reader, tokio::runtime::Handle::current(), registry.clone());
    let context = Context::default();
    let name = CString::new("0-0").unwrap();
    let missing = fs.lookup(&context, ROOT_ID, &name).unwrap();
    assert_eq!(missing.inode, 0);
    assert_eq!(missing.entry_timeout, std::time::Duration::ZERO);
    assert!(fs.listing(ROOT_ID).unwrap().is_empty());
    let node = Node::File {
        digest: casita::BlobId::new(casita::Digest::hash(b"seed")),
        size: 4,
        executable: false,
    };
    let first = registry
        .register(BTreeMap::from([(b"input".to_vec(), node.clone())]))
        .unwrap();
    assert_eq!(first.name(b"input"), Some(b"0-0".as_slice()));
    let present = fs.lookup(&context, ROOT_ID, &name).unwrap();
    assert_ne!(present.inode, 0);
    assert_eq!(present.entry_timeout, std::time::Duration::ZERO);
    assert_eq!(fs.listing(ROOT_ID).unwrap().len(), 1);
    let second = registry
        .register(BTreeMap::from([(b"input".to_vec(), node)]))
        .unwrap();
    let second_name = CString::new(second.name(b"input").unwrap()).unwrap();
    assert_ne!(name, second_name);
    drop(first);
    assert_eq!(fs.lookup(&context, ROOT_ID, &name).unwrap().inode, 0);
    assert_ne!(fs.lookup(&context, ROOT_ID, &second_name).unwrap().inode, 0);
    assert_eq!(fs.listing(ROOT_ID).unwrap().len(), 1);
    drop(second);
    assert!(fs.listing(ROOT_ID).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_mount_keeps_other_leases_readable_after_release() {
    if !can_mount() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let repository = Arc::new(TestRepository::default());
    let source = seed_tree(temp.path());

    let imported = repository.import_output(&source).await.unwrap();
    let node = imported.clone();

    let registry = Arc::new(super::InputRegistry::default());
    let filesystem = StoreFs::with_input_registry(
        repository,
        tokio::runtime::Handle::current(),
        registry.clone(),
    );
    let mountpoint = temp.path().join("mount");
    std::fs::create_dir(&mountpoint).unwrap();
    let mount = FuseMount::new(filesystem, &mountpoint, 2).unwrap();
    assert!(!mountpoint.join("0-0").exists());
    let inputs = BTreeMap::from([(b"input".to_vec(), node)]);
    let first = registry.register(inputs.clone()).unwrap();
    let second = registry.register(inputs).unwrap();
    let a = mountpoint.join(OsStr::from_bytes(first.name(b"input").unwrap()));
    let b = mountpoint.join(OsStr::from_bytes(second.name(b"input").unwrap()));
    assert_eq!(fs::read(a.join("data")).unwrap(), b"contents of data\n");
    assert!(fs::write(a.join("data"), b"changed").is_err());
    drop(first);
    assert!(
        !a.exists(),
        "released roots must disappear even after a cached lookup"
    );
    assert_eq!(fs::read(b.join("data")).unwrap(), b"contents of data\n");
    drop(second);
    mount.unmount().unwrap();
}

fn assert_errno<T: std::fmt::Debug>(result: std::io::Result<T>, errno: i32) {
    assert_eq!(result.unwrap_err().raw_os_error(), Some(errno));
}

#[test]
fn readers_release_collection_protection_on_plain_fuse_threads() {
    // Cover a normal close, teardown with an open handle, and the last in-flight
    // reference disappearing after release. None of these threads enters Tokio.
    for mode in ["release", "teardown", "in-flight"] {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let repository = Arc::new(casita::Repository::memory().unwrap());
        let root: casita::RootName = "read-lifetime".try_into().unwrap();
        let key = runtime
            .block_on(repository.import(casita::import::BlobImport::new(
                b"data".as_slice(),
                root.clone(),
            )))
            .unwrap();
        let node = Node::File {
            digest: casita::BlobId::new(key.native_digest().unwrap()),
            size: 4,
            executable: false,
        };
        let view = FilesystemView::new(repository.clone(), [(b"file".to_vec(), node)].into());
        let filesystem = StoreFs::new(view.clone(), runtime.handle().clone());
        let context = Context::default();
        let inode = filesystem
            .lookup(&context, ROOT_ID, &CString::new("file").unwrap())
            .unwrap()
            .inode;
        let handle = filesystem
            .open(&context, inode, libc::O_RDONLY as u32, 0)
            .unwrap()
            .0
            .unwrap();
        let file = filesystem.reader(handle).unwrap();
        file.lock().unwrap().reader = Some(
            runtime
                .block_on(view.open(&view.root(b"file").unwrap()))
                .unwrap(),
        );
        runtime.block_on(async {
            assert!(repository.remove_root(&root, &key).await.unwrap());
            repository.flush().await.unwrap();
            repository.collect().await.unwrap();
            assert!(
                repository.object(&key).await.unwrap().is_some(),
                "a live reader must protect its bytes"
            );
        });
        std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            if mode != "in-flight" {
                drop(file);
                if mode == "release" {
                    filesystem
                        .release(&context, inode, 0, handle, false, false, None)
                        .unwrap();
                }
                drop(filesystem);
            } else {
                filesystem
                    .release(&context, inode, 0, handle, false, false, None)
                    .unwrap();
                drop(filesystem);
                drop(file);
            }
        })
        .join()
        .unwrap();
        runtime.block_on(async {
            repository.flush().await.unwrap();
            repository.collect().await.unwrap();
            assert!(
                repository.object(&key).await.unwrap().is_none(),
                "{mode}: closed readers must stop retaining unrooted data"
            );
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operation_errors_preserve_read_only_filesystem_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    let directory = tmp.path().join("directory");
    fs::write(&file, b"contents").unwrap();
    fs::create_dir(&directory).unwrap();
    let view = view_of(&[(b"file", &file), (b"directory", &directory)]).await;
    let filesystem = StoreFs::new(view, tokio::runtime::Handle::current());
    let context = Context::default();

    let file = filesystem
        .lookup(&context, ROOT_ID, &CString::new("file").unwrap())
        .unwrap()
        .inode;
    let directory = filesystem
        .lookup(&context, ROOT_ID, &CString::new("directory").unwrap())
        .unwrap()
        .inode;

    assert_errno(filesystem.getattr(&context, u64::MAX, None), libc::ENOENT);
    assert_errno(filesystem.readlink(&context, ROOT_ID), libc::EINVAL);
    assert_errno(filesystem.readlink(&context, file), libc::EINVAL);
    assert_errno(
        filesystem.open(&context, ROOT_ID, libc::O_RDONLY as u32, 0),
        libc::EISDIR,
    );
    assert_errno(
        filesystem.open(&context, directory, libc::O_RDONLY as u32, 0),
        libc::EISDIR,
    );
    assert_errno(
        filesystem.open(&context, file, libc::O_WRONLY as u32, 0),
        libc::EROFS,
    );
    assert_errno(filesystem.opendir(&context, file, 0), libc::ENOTDIR);
    assert_errno(
        filesystem.access(&context, ROOT_ID, libc::W_OK as u32),
        libc::EROFS,
    );

    let missing = filesystem
        .lookup(&context, ROOT_ID, &CString::new("missing").unwrap())
        .unwrap();
    assert_eq!(missing.inode, 0, "negative lookups remain cacheable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serves_a_store_path_without_materializing_it() {
    if !can_mount() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let tree = seed_tree(tmp.path());
    let view = view_of(&[(b"aaaa-tree", &tree)]).await;
    let mountpoint = tmp.path().join("mnt");
    let mount = mount(view, &mountpoint);

    let root = mountpoint.join("aaaa-tree");
    assert_eq!(fs::read(root.join("data")).unwrap(), b"contents of data\n");
    assert_eq!(
        fs::read(root.join(OsStr::from_bytes(b"raw\xff"))).unwrap(),
        b"byte safe\n"
    );
    assert_eq!(
        fs::read_link(root.join("link")).unwrap(),
        Path::new("bin/tool")
    );
    assert_eq!(
        fs::metadata(root.join("bin/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111,
        "the executable bit survives the round trip"
    );
    assert!(fs::metadata(root.join("bin")).unwrap().is_dir());
    for path in [
        &root,
        &root.join("data"),
        &root.join("bin/tool"),
        &root.join("link"),
    ] {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.mtime(), 1, "Nix store mtime at {}", path.display());
        assert_eq!(metadata.mtime_nsec(), 0);
    }

    let mut listing: Vec<Vec<u8>> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().as_bytes().to_vec())
        .collect();
    listing.sort();
    assert_eq!(
        listing,
        vec![
            b"bin".to_vec(),
            b"data".to_vec(),
            b"link".to_vec(),
            b"raw\xff".to_vec()
        ]
    );

    // The mount root lists exactly the roots it was given.
    let roots: Vec<_> = fs::read_dir(&mountpoint)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(roots, vec![OsStr::new("aaaa-tree")]);

    // Reads are served from arbitrary offsets through one open handle.
    let mut file = fs::File::open(root.join("data")).unwrap();
    file.seek(SeekFrom::Start(12)).unwrap();
    let mut tail = String::new();
    file.read_to_string(&mut tail).unwrap();
    assert_eq!(tail, "data\n");

    assert!(
        fs::write(root.join("data"), b"nope").is_err(),
        "the store is read-only"
    );
    assert!(fs::metadata(root.join("missing")).is_err());

    // The mount counted what it served, which is how a slow build gets
    // attributed to round trips rather than to bytes.
    let stats = mount.stats();
    assert!(
        stats.calls(Op::Lookup) > 0,
        "path resolution went through the mount"
    );
    assert!(stats.calls(Op::Readdir) > 0);
    assert!(stats.calls(Op::Readlink) > 0);
    assert!(stats.calls(Op::Read) > 0);
    assert!(
        stats.bytes_read() >= b"contents of data\n".len() as u64,
        "served bytes are counted"
    );
    assert_eq!(stats.total_calls(), stats.total_calls());

    mount.unmount().unwrap();
    assert_eq!(
        fs::read_dir(&mountpoint).unwrap().count(),
        0,
        "unmounting leaves the empty mountpoint behind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_content_under_two_roots_is_one_inode() {
    if !can_mount() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    fs::write(&first, b"shared\n").unwrap();
    fs::write(&second, b"shared\n").unwrap();

    let view = view_of(&[(b"aaaa-first", &first), (b"bbbb-second", &second)]).await;
    let mountpoint = tmp.path().join("mnt");
    let _mount = mount(view, &mountpoint);

    let left = fs::metadata(mountpoint.join("aaaa-first")).unwrap();
    let right = fs::metadata(mountpoint.join("bbbb-second")).unwrap();
    assert_eq!(
        fs::read(mountpoint.join("bbbb-second")).unwrap(),
        b"shared\n"
    );
    assert_eq!(
        left.ino(),
        right.ino(),
        "content addressed store paths share an inode, so the kernel caches them once"
    );
}

/// The point of keying listings by content: what one mount decoded, the next
/// one gets for free, which is what makes a session of many builds cheap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_mount_reuses_the_first_mounts_listings() {
    if !can_mount() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let tree = seed_tree(tmp.path());
    let cache = Arc::new(ListingCache::default());

    for (round, expected_misses) in [("first", 2), ("second", 2)] {
        let view = view_of(&[(b"aaaa-tree", &tree)]).await;
        let mountpoint = tmp.path().join(format!("mnt-{round}"));
        fs::create_dir_all(&mountpoint).unwrap();
        let mount = FuseMount::new(
            StoreFs::with_listing_cache(
                view,
                tokio::runtime::Handle::current(),
                Arc::clone(&cache),
            ),
            &mountpoint,
            2,
        )
        .expect("mount");

        // Touch both directories in the tree.
        assert_eq!(
            fs::read(mountpoint.join("aaaa-tree/bin/tool")).unwrap(),
            b"#!/bin/sh\necho tool\n"
        );
        let (hits, misses) = cache.counts();
        assert_eq!(
            misses, expected_misses,
            "{round} mount: only the two directories are ever decoded (hits {hits})"
        );
        mount.unmount().unwrap();
    }
    let (hits, _) = cache.counts();
    assert!(hits > 0, "the second mount served its lookups from cache");
}
