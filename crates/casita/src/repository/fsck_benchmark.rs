//! Untimed, exclusive fixture construction for `benchmark run fsck`.
//!
//! Use real packed payloads, verified records, and a coordinated catalog, but
//! no online mutation session: the new fixture has no concurrent readers or
//! collectors. This keeps pin-ledger staging costs out of fsck investigations.

use super::*;

#[tokio::test]
#[ignore = "fixture builder for the permanent fsck benchmark corpus"]
async fn benchmark_seed_fsck_fixture() {
    let path = std::path::PathBuf::from(std::env::var("CASITA_BENCH_FSCK_REPOSITORY").unwrap());
    let files: usize = std::env::var("CASITA_BENCH_FSCK_FILES")
        .unwrap()
        .parse()
        .unwrap();
    assert!(files > 0);
    // Refuse to seed any existing repository, including a partially built one.
    std::fs::create_dir(&path).unwrap();
    let branches = files.div_ceil(1024).max(64).min(files);
    let mut directories = vec![crate::Directory::new(); branches];
    let repository = Repository::local(&path).await.unwrap();
    let batch = repository.payloads.begin_batch();
    let formats = crate::FormatRegistry::builtin();
    let limits = crate::FormatLimits::default();
    let mut mutation = MetadataMutation::new();
    for index in 0..files {
        let bytes = format!("retained object {index}\n").into_bytes();
        let digest = repository.payloads.put_slice(&bytes).await.unwrap();
        let key = ObjectKey::blob(digest);
        let verified = formats
            .verify(&key, &mut std::io::Cursor::new(&bytes), &limits)
            .await
            .unwrap();
        mutation.add_object(verified);
        directories[index % branches]
            .add(
                format!("node-{index:08}").try_into().unwrap(),
                crate::Node::File {
                    digest,
                    size: bytes.len() as u64,
                    executable: false,
                },
            )
            .unwrap();
    }
    let mut root = crate::Directory::new();
    for (index, directory) in directories.into_iter().enumerate() {
        let bytes = directory.encode();
        repository.payloads.put_slice(&bytes).await.unwrap();
        let key = ObjectKey::directory(directory.digest());
        let verified = formats
            .verify(&key, &mut std::io::Cursor::new(bytes), &limits)
            .await
            .unwrap();
        mutation.add_object(verified);
        root.add(
            format!("branch-{index:02}").try_into().unwrap(),
            crate::Node::Directory {
                digest: directory.digest(),
                size: directory.size(),
            },
        )
        .unwrap();
    }
    let bytes = root.encode();
    repository.payloads.put_slice(&bytes).await.unwrap();
    let root_key = ObjectKey::directory(root.digest());
    mutation.add_object(
        formats
            .verify(&root_key, &mut std::io::Cursor::new(bytes), &limits)
            .await
            .unwrap(),
    );
    mutation.set_root("bench/retained".parse().unwrap(), root_key);
    let catalog = repository
        .payloads
        .publication()
        .prepare_state_commit()
        .await
        .unwrap();
    mutation.set_payload_catalog(
        catalog
            .catalog()
            .expect("local packed fixture has a payload catalog")
            .to_vec(),
    );
    let revision = repository.state.snapshot().await.unwrap().revision();
    repository.state.commit(&revision, mutation).await.unwrap();
    catalog.commit().unwrap();
    drop(batch);
    drop(repository);
    println!(
        "fsck_fixture {}",
        serde_json::json!({
            "files": files, "objects": files + branches + 1,
            "branches": branches, "repository": path,
        })
    );
}
