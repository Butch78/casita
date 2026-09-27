#![cfg(feature = "cli")]

use casita::{Repository, RootName};
use std::process::Command;

#[tokio::test]
async fn verified_cat_local_and_transfer_source_emit_only_authenticated_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = Repository::local(directory.path()).await.unwrap();
    let bytes: Vec<u8> = (0..131073).map(|i| (i * 17) as u8).collect();
    let key = repository
        .import(casita::import::BlobImport::new(
            bytes.as_slice(),
            RootName::try_from("file").unwrap(),
        ))
        .await
        .unwrap();
    repository.flush().await.unwrap();
    drop(repository);

    let local = Command::new(env!("CARGO_BIN_EXE_casita"))
        .arg("--repository")
        .arg(directory.path())
        .args(["cat", "--verified", &key.to_string()])
        .output()
        .unwrap();
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    assert_eq!(local.stdout, bytes);
    let transferred = Command::new(env!("CARGO_BIN_EXE_casita"))
        .args(["cat", "--verified", "--from"])
        .arg(directory.path())
        .arg(key.to_string())
        .output()
        .unwrap();
    assert!(
        transferred.status.success(),
        "{}",
        String::from_utf8_lossy(&transferred.stderr)
    );
    assert_eq!(transferred.stdout, bytes);

    // Imported proofs live in packs. Corrupt the stored pack so verified reads
    // must reject it before releasing any unauthenticated bytes.
    let packs: Vec<_> = std::fs::read_dir(directory.path().join("blobs/bao-packs/b3"))
        .unwrap()
        .flat_map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap())
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(packs.len(), 1);
    std::fs::write(&packs[0], b"corrupt").unwrap();
    let corrupt = Command::new(env!("CARGO_BIN_EXE_casita"))
        .arg("--repository")
        .arg(directory.path())
        .args(["cat", "--verified", &key.to_string()])
        .output()
        .unwrap();
    assert!(!corrupt.status.success());
    assert!(corrupt.stdout.is_empty());
}
