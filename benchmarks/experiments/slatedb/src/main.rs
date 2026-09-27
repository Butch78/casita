//! A capability probe, not a Casita backend or an equivalent throughput test.
use std::sync::Arc;
use std::time::{Duration, Instant};

use slatedb::object_store::memory::InMemory;
use slatedb::{Db, WriteBatch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let storage = Arc::new(InMemory::new());
    let first = Db::open("casita-capabilities", storage.clone()).await?;
    let before = first.snapshot().await?;
    let mut batch = WriteBatch::new();
    batch.put(b"objects/one", b"immutable record");
    batch.put(b"roots/main", b"objects/one");
    let started = Instant::now();
    first.write(batch).await?;
    first.flush().await?;
    let publish_nanos = started.elapsed().as_nanos();
    assert!(before.get(b"objects/one").await?.is_none());
    assert!(before.get(b"roots/main").await?.is_none());
    let after = first.snapshot().await?;
    assert_eq!(
        after.get(b"roots/main").await?.unwrap().as_ref(),
        b"objects/one"
    );
    assert_eq!(
        after.get(b"objects/one").await?.unwrap().as_ref(),
        b"immutable record"
    );
    drop(before);
    drop(after);

    // Casita permits independently opened competing state writers, returning
    // StaleRevision while keeping both handles usable. Test whether a second
    // SlateDB writer has that contract or takes ownership from the first.
    let second = Db::open("casita-capabilities", storage).await?;
    assert!(second.get(b"objects/one").await?.is_some());
    let stale_write = tokio::time::timeout(Duration::from_secs(30), async {
        first.put(b"objects/two", b"second record").await?;
        first.flush().await
    })
    .await?;
    assert!(
        stale_write.is_err(),
        "a second writer must fence the old writer"
    );
    let stale_error = stale_write.unwrap_err().to_string();
    second.put(b"objects/three", b"third record").await?;
    second.flush().await?;
    second.close().await?;
    let _ = first.close().await;
    println!(
        "{}",
        serde_json::json!({
            "experiment": "slatedb-capabilities", "slatedb_version": "0.16.0",
            "storage": "in-memory object store", "atomic_batch": true,
            "snapshot_isolation": true, "second_writer_fences_first": true,
            "first_writer_error": stale_error, "batch_publish_nanos": publish_nanos,
            "casita_multiwriter_contract_compatible": false,
            "interpretation": "Capability probe only: no Casita formats, verification, retention, or physical GC adapter",
        })
    );
    Ok(())
}
