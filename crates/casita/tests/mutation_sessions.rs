#![cfg(all(feature = "native", feature = "experimental"))]

use casita::ObjectKey;
use casita::{
    experimental::{ClosureStatus, ObjectRequest, Repository, TransferRequest},
    import::UnrootedFilesystemImport,
};

#[tokio::test]
async fn caller_owned_transfer_retains_unrooted_checkpoints() {
    let source = Repository::memory().unwrap().into_erased();
    let destination = Repository::memory().unwrap().into_erased();
    let writing = source.mutation_session().await.unwrap();
    let staged = writing.stage_blob(b"session transfer").await.unwrap();
    let key = staged.record().key().clone();
    writing.publish_unrooted(vec![staged]).await.unwrap();
    let receiving = destination.mutation_session().await.unwrap();
    receiving
        .transfer_from(
            &source,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: key.clone(),
                    recursive: true,
                }],
                roots: vec![],
            },
        )
        .await
        .unwrap();
    destination.collect().await.unwrap();
    assert!(matches!(
        destination.verify_closure(&key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    drop(receiving);
    destination.collect().await.unwrap();
    assert!(matches!(
        destination.verify_closure(&key).await.unwrap(),
        ClosureStatus::Missing { .. }
    ));
}

#[tokio::test]
async fn caller_owned_transfer_retains_reused_objects() {
    let source = Repository::memory().unwrap().into_erased();
    let writing = source.mutation_session().await.unwrap();
    let bytes = b"reused session transfer";
    let staged = writing.stage_blob(bytes).await.unwrap();
    let key = staged.record().key().clone();
    writing.publish_unrooted(vec![staged]).await.unwrap();

    for local in [false, true] {
        for recursive in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let destination = if local {
                Repository::local(directory.path())
                    .await
                    .unwrap()
                    .into_erased()
            } else {
                Repository::memory().unwrap().into_erased()
            };
            let request = TransferRequest {
                objects: vec![ObjectRequest {
                    key: key.clone(),
                    recursive,
                }],
                roots: vec![],
            };
            let first = destination.mutation_session().await.unwrap();
            first.transfer_from(&source, request.clone()).await.unwrap();
            let second = destination.mutation_session().await.unwrap();
            let result = second.transfer_from(&source, request).await.unwrap();
            assert_eq!(result.progress.published_objects, 0);
            drop(first);

            destination.collect().await.unwrap();
            assert!(matches!(
                destination.verify_closure(&key).await.unwrap(),
                ClosureStatus::Complete { .. }
            ));
            let hold = destination.retention_hold().await.unwrap();
            let (_, mut reader) = hold.open_payload(&key).await.unwrap().unwrap();
            let mut actual = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut actual)
                .await
                .unwrap();
            assert_eq!(actual, bytes);
            drop(reader);
            drop(hold);
            drop(second);

            destination.collect().await.unwrap();
            assert!(matches!(
                destination.verify_closure(&key).await.unwrap(),
                ClosureStatus::Missing { .. }
            ));
        }
    }
}

#[tokio::test]
async fn unrooted_filesystem_import_creates_no_permanent_name() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("file"), b"imported file").unwrap();
    let repository = Repository::memory().unwrap().into_erased();
    let session = repository.mutation_session().await.unwrap();
    let key: ObjectKey = session
        .import(UnrootedFilesystemImport::new(directory.path()))
        .await
        .unwrap();
    repository.collect().await.unwrap();
    assert!(matches!(
        repository.verify_closure(&key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    drop(session);
    repository.collect().await.unwrap();
    assert!(matches!(
        repository.verify_closure(&key).await.unwrap(),
        ClosureStatus::Missing { .. }
    ));
}
#[tokio::test]
async fn chunk_presence_preserves_duplicates_and_missing_entries() {
    use casita::experimental::TransferSource;
    let directory = tempfile::tempdir().unwrap();
    let repository = Repository::local(directory.path())
        .await
        .unwrap()
        .into_erased();
    let session = repository.mutation_session().await.unwrap();
    let staged = session
        .stage_blob(b"physical chunk presence")
        .await
        .unwrap();
    let key = staged.record().key().clone();
    session.publish_unrooted(vec![staged]).await.unwrap();
    let source = repository
        .begin_transfer(casita::experimental::TransferSelection::Snapshot)
        .await
        .unwrap();
    let record = source.object(&key).await.unwrap().unwrap();
    let chunks = source.chunks(&record).await.unwrap().unwrap();
    let existing = chunks[0].digest;
    let missing = casita::experimental::ChunkId::new(casita::Digest::hash(b"absent chunk"));
    assert_eq!(
        source
            .as_chunk_source()
            .unwrap()
            .has_chunks(&[existing, missing, existing])
            .await
            .unwrap(),
        vec![true, false, true]
    );
}
