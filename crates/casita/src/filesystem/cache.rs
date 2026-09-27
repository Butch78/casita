//! The local record of which content each imported file last held.
//!
//! Re-importing a tree that has not changed should cost a stat per file, not a
//! read per file. That is only possible if the repository remembers what it
//! saw: this table maps a file's identity, as the walk observed it, to the
//! blob its content produced.
//!
//! What that buys is a trust assumption, and it is the same one git, restic
//! and borg make. Content is recognized by its device, inode, size and both
//! timestamps rather than by reading it. A file rewritten in place while
//! keeping every one of those identical would be read as unchanged. `ctime` is
//! part of the comparison because userspace cannot set it, so restoring a
//! modification time is not enough to hide a rewrite; only a deliberately
//! manipulated clock is. Callers that cannot accept the assumption re-read
//! everything, which is what `--rehash` is for.
//!
//! Nothing here is repository state. The table is local, it is never consulted
//! by a reader, and every recorded digest is confirmed against committed state
//! before it is used, so an entry that outlived its object is a wasted lookup
//! rather than a wrong answer.

use std::sync::Arc;

use async_trait::async_trait;
use turso::params;

use crate::digest::{BlobId, Digest};
use crate::error::Error;
use crate::filesystem::IngestCache;
use crate::filesystem::root::FileIdentity;
use crate::sqlite::TursoDb;

/// The ingest cache kept in the repository's own database.
pub(crate) struct TursoIngestCache {
    db: Arc<TursoDb>,
}

impl TursoIngestCache {
    pub(crate) fn new(db: Arc<TursoDb>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl IngestCache for TursoIngestCache {
    #[tracing::instrument(
        name = "filesystem.ingest_cache.recall",
        level = "debug",
        skip_all,
        fields(files = files.len())
    )]
    async fn recall(&self, files: &[FileIdentity]) -> Result<Vec<Option<BlobId>>, Error> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let files = files.to_vec();
        // Reuse the serialized connection for each bounded walk page. Keep
        // the whole page in one read transaction so each file does not reopen
        // a WAL snapshot as publication checkpoints accumulate.
        let found = self
            .db
            .write(move |connection| {
                Box::pin(async move {
                    let transaction = connection
                        .transaction_with_behavior(
                            turso::transaction::TransactionBehavior::Deferred,
                        )
                        .await?;
                    let mut statement = transaction
                        .prepare_cached(
                            "SELECT blob_digest FROM ingest_cache \
                             WHERE device = ?1 AND inode = ?2 AND size = ?3 \
                               AND mtime_sec = ?4 AND mtime_nsec = ?5 \
                               AND ctime_sec = ?6 AND ctime_nsec = ?7",
                        )
                        .await?;
                    let mut found = Vec::with_capacity(files.len());
                    for file in &files {
                        let mut rows = statement
                            .query(params![
                                file.device as i64,
                                file.inode as i64,
                                file.size as i64,
                                file.mtime_sec,
                                file.mtime_nsec,
                                file.ctime_sec,
                                file.ctime_nsec,
                            ])
                            .await?;
                        found.push(match rows.next().await? {
                            Some(row) => {
                                let stored: Vec<u8> = row.get(0)?;
                                Some(BlobId::new(
                                    Digest::try_from(stored.as_slice())
                                        .map_err(|error| Error::from(error.to_string()))?,
                                ))
                            }
                            None => None,
                        });
                    }
                    drop(statement);
                    transaction.commit().await?;
                    Ok(found)
                })
            })
            .await?;
        tracing::debug!(
            hits = found.iter().filter(|entry| entry.is_some()).count(),
            misses = found.iter().filter(|entry| entry.is_none()).count(),
            "filesystem ingest cache lookup completed"
        );
        Ok(found)
    }

    #[tracing::instrument(
        name = "filesystem.ingest_cache.remember",
        level = "debug",
        skip_all,
        fields(files = files.len())
    )]
    async fn remember(&self, files: &[(FileIdentity, BlobId)]) -> Result<(), Error> {
        if files.is_empty() {
            return Ok(());
        }
        let files = files.to_vec();
        self.db
            .write(move |connection| {
                Box::pin(async move {
                    let transaction = connection.transaction().await?;
                    let mut statement = transaction
                        .prepare_cached(
                            "INSERT OR REPLACE INTO ingest_cache \
                             (device, inode, size, mtime_sec, mtime_nsec, \
                              ctime_sec, ctime_nsec, blob_digest) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        )
                        .await?;
                    for (file, digest) in &files {
                        statement
                            .execute(params![
                                file.device as i64,
                                file.inode as i64,
                                file.size as i64,
                                file.mtime_sec,
                                file.mtime_nsec,
                                file.ctime_sec,
                                file.ctime_nsec,
                                digest.digest().as_bytes().as_slice(),
                            ])
                            .await?;
                    }
                    transaction.commit().await?;
                    Ok(())
                })
            })
            .await
    }
}
