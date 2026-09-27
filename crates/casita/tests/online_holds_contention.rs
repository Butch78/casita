#![cfg(all(feature = "native", feature = "experimental"))]

use casita::{
    RetryDisposition,
    experimental::{MetadataStore, Repository, flush_repository_leases},
    import::FilesystemImport,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::io::AsyncReadExt;

/// Exercise the import/reader/collector workload which exhausted local ledger
/// CAS retries. No failed import or read is retried by this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_imports_with_readers_and_gc_preserve_payloads_and_release_pins() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo");
        let repo = Repository::local(&path).await.unwrap();
        // Independent handles must coordinate through the durable ledger.
        let reader_repo = Repository::local(&path).await.unwrap();
        let collector_repo = Repository::local(&path).await.unwrap();
        let input = temp.path().join("input");
        std::fs::create_dir(&input).unwrap();
        // Seventeen directories exercise concurrent directory staging and
        // its next window while independent readers and GC use the ledger.
        for file in 0..8 {
            std::fs::create_dir_all(input.join(file.to_string()).join("nested")).unwrap();
        }
        let seed = repo.mutation_session().await.unwrap();
        let staged = seed.stage_blob(b"sentinel").await.unwrap();
        let key = staged.record().key().clone();
        seed.publish_rooted(vec![staged], "sentinel".parse().unwrap(), key.clone())
            .await
            .unwrap();
        drop(seed);
        flush_repository_leases().await.unwrap();
        let done = AtomicBool::new(false);
        let writer = async {
            let mut latest = None;
            for iteration in 0..12 {
                for file in 0..8 {
                    std::fs::write(
                        input.join(file.to_string()).join("nested/file"),
                        vec![(iteration * 8 + file) as u8; 4096],
                    )
                    .unwrap();
                }
                latest = Some(
                    repo.import(
                        FilesystemImport::new(input.clone(), "import".parse().unwrap())
                            .with_file_concurrency(std::num::NonZeroUsize::new(16).unwrap())
                            .reread(true),
                    )
                    .await
                    .unwrap(),
                );
            }
            done.store(true, Ordering::Release);
            latest.unwrap()
        };
        let reader = async {
            let mut reads = 0;
            while !done.load(Ordering::Acquire) {
                let hold = reader_repo.retention_hold().await.unwrap();
                let (_, mut stream) = hold.open_payload(&key).await.unwrap().unwrap();
                drop(hold);
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(bytes, b"sentinel");
                reads += 1;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            reads
        };
        let collector = async {
            let mut attempts = 0;
            while !done.load(Ordering::Acquire) {
                if let Err(error) = collector_repo.try_collect().await {
                    assert_eq!(
                        error.retry_disposition(),
                        RetryDisposition::Retry,
                        "{error:?}"
                    );
                }
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            attempts
        };
        let (latest, reads, attempts) = tokio::join!(writer, reader, collector);
        assert!(reads > 0 && attempts > 0);
        flush_repository_leases().await.unwrap();
        repo.collect().await.unwrap();
        flush_repository_leases().await.unwrap();
        let inventory = repo
            .metadata()
            .pin_store()
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap();
        assert!(inventory.pins.is_empty());
        assert!(inventory.deletions.is_empty());
        assert!(inventory.collector.is_none() && inventory.logical_prune.is_none());
        let checkout = temp.path().join("checkout");
        repo.checkout(&latest, &checkout).await.unwrap();
        for file in 0..8 {
            assert_eq!(
                std::fs::read(checkout.join(file.to_string()).join("nested/file")).unwrap(),
                vec![(11 * 8 + file) as u8; 4096]
            );
        }
        assert!(repo.fsck().await.unwrap().is_healthy());
        flush_repository_leases().await.unwrap();
    })
    .await
    .expect("online operations must finish under contention");
}
