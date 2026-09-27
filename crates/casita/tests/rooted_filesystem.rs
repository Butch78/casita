#![cfg(feature = "experimental")]

//! Containment of filesystem import and materialization at the public API.
//!
//! The unit tests beside the primitive cover component-level swaps. These drive
//! the shipped entry points (`import`, `checkout`, and the native Git
//! checkout) against links that try to redirect them, and assert both halves of
//! the guarantee: the operation fails or stays inside, and nothing appears
//! outside the selected root.

#![cfg(feature = "native")]

use std::path::{Path, PathBuf};

use casita::experimental::{Repository, RootName};

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

/// A repository, a source tree, and an outside directory holding a secret that
/// no import may read and no checkout may overwrite.
struct Playground {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    outside: PathBuf,
}

impl Playground {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repository");
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"not yours").unwrap();
        Self {
            _temp: temp,
            root,
            source,
            outside,
        }
    }

    async fn repository(
        &self,
    ) -> Repository<casita::experimental::ChunkedBlobStore, casita::experimental::TursoMetadataStore>
    {
        Repository::local(&self.root).await.unwrap()
    }

    /// Whether the outside directory still holds exactly its secret.
    fn outside_untouched(&self) -> bool {
        std::fs::read(self.outside.join("secret")).unwrap() == b"not yours"
            && std::fs::read_dir(&self.outside).unwrap().count() == 1
    }
}

fn name(literal: &str) -> RootName {
    RootName::try_from(literal).unwrap()
}

#[tokio::test]
async fn import_records_a_linked_directory_instead_of_reading_through_it() {
    let play = Playground::new();
    std::fs::write(play.source.join("own"), b"mine").unwrap();
    link_dir(&play.outside, &play.source.join("elsewhere"));

    let repository = play.repository().await;
    let root = repository
        .import(casita::import::FilesystemImport::new(
            &play.source,
            name("trees/linked"),
        ))
        .await
        .unwrap();

    // The ingested tree names the link as a link. Nothing behind it was read,
    // so the secret has no object of its own and the tree has no third entry.
    let directory = read_directory(&repository, &root).await;
    assert_eq!(directory.len(), 2);
    assert!(matches!(
        directory.get("elsewhere"),
        Some(casita::experimental::Node::Symlink { .. })
    ));
    assert!(matches!(
        directory.get("own"),
        Some(casita::experimental::Node::File { .. })
    ));
    assert!(play.outside_untouched());
}

/// Decode the canonical directory an import produced.
async fn read_directory(
    repository: &Repository<
        casita::experimental::ChunkedBlobStore,
        casita::experimental::TursoMetadataStore,
    >,
    key: &casita::experimental::ObjectKey,
) -> casita::experimental::Directory {
    use tokio::io::AsyncReadExt as _;

    let hold = repository.retention_hold().await.unwrap();
    let (_, mut reader) = hold.open_payload(key).await.unwrap().unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.unwrap();
    casita::experimental::Directory::decode(&bytes).unwrap()
}

#[tokio::test]
async fn import_refuses_a_linked_root() {
    let play = Playground::new();
    let linked_root = play.source.parent().unwrap().join("linked-source");
    link_dir(&play.outside, &linked_root);

    let repository = play.repository().await;
    assert!(
        repository
            .import(casita::import::FilesystemImport::new(
                &linked_root,
                name("trees/linked-root")
            ))
            .await
            .is_err()
    );
    assert!(play.outside_untouched());
}

#[tokio::test]
async fn checkout_refuses_a_linked_target() {
    let play = Playground::new();
    std::fs::write(play.source.join("file"), b"payload").unwrap();
    let repository = play.repository().await;
    let root = repository
        .import(casita::import::FilesystemImport::new(
            &play.source,
            name("trees/plain"),
        ))
        .await
        .unwrap();

    let linked_target = play.source.parent().unwrap().join("linked-target");
    link_dir(&play.outside, &linked_target);

    assert!(repository.checkout(&root, &linked_target).await.is_err());
    assert!(play.outside_untouched());
}

#[tokio::test]
async fn checkout_refuses_a_target_that_already_holds_content() {
    let play = Playground::new();
    std::fs::write(play.source.join("file"), b"payload").unwrap();
    let repository = play.repository().await;
    let root = repository
        .import(casita::import::FilesystemImport::new(
            &play.source,
            name("trees/plain"),
        ))
        .await
        .unwrap();

    let target = play.source.parent().unwrap().join("occupied");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("existing"), b"do not clobber").unwrap();

    assert!(repository.checkout(&root, &target).await.is_err());
    assert_eq!(
        std::fs::read(target.join("existing")).unwrap(),
        b"do not clobber"
    );
}

#[tokio::test]
async fn checkout_materializes_an_escaping_link_without_following_it() {
    let play = Playground::new();
    std::fs::create_dir(play.source.join("nested")).unwrap();
    std::fs::write(play.source.join("nested/file"), b"payload").unwrap();
    // A link that points out of the tree is data: it is stored and restored as
    // written, and materializing it neither reads nor writes the target.
    #[cfg(unix)]
    std::os::unix::fs::symlink("../../outside/secret", play.source.join("nested/escape")).unwrap();

    let repository = play.repository().await;
    let root = repository
        .import(casita::import::FilesystemImport::new(
            &play.source,
            name("trees/escaping"),
        ))
        .await
        .unwrap();
    let target = play.source.parent().unwrap().join("checkout");
    repository.checkout(&root, &target).await.unwrap();

    assert_eq!(
        std::fs::read(target.join("nested/file")).unwrap(),
        b"payload"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(target.join("nested/escape")).unwrap(),
        Path::new("../../outside/secret")
    );
    assert!(play.outside_untouched());
}
