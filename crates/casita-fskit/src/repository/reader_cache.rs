//! Bounded reader reuse. Only successfully parked readers return to a cache slot.
use super::{Diagnostics, Reader};
use anyhow::{anyhow, Result};
use casita_fs::{ContentKey, FilesystemNode};
use std::{
    collections::VecDeque,
    io,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Instant,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    runtime::Runtime,
};

type Slot = Arc<Mutex<Option<casita::Reader>>>;

pub(super) struct ReaderCache {
    capacity: usize,
    trace_reads: bool,
    slots: Mutex<VecDeque<(u64, Slot)>>,
}

impl ReaderCache {
    pub(super) fn new(capacity: usize, trace_reads: bool) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            trace_reads,
            slots: Mutex::new(VecDeque::new()),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }

    pub(super) fn clear(&self, runtime: &Runtime) {
        let _entered = runtime.enter();
        self.slots.lock().unwrap().clear();
    }

    fn slot(&self, id: u64, counters: &Diagnostics) -> Slot {
        let mut slots = self.slots.lock().unwrap();
        if let Some(index) = slots.iter().position(|(key, _)| *key == id) {
            counters.reader_hits.fetch_add(1, Ordering::Relaxed);
            let entry = slots.remove(index).unwrap();
            let slot = Arc::clone(&entry.1);
            slots.push_back(entry);
            slot
        } else {
            counters.reader_misses.fetch_add(1, Ordering::Relaxed);
            if slots.len() == self.capacity {
                slots.pop_front();
                counters.reader_evictions.fetch_add(1, Ordering::Relaxed);
            }
            let slot = Arc::new(Mutex::new(None));
            slots.push_back((id, Arc::clone(&slot)));
            slot
        }
    }

    /// Serialize reads for a cached file without holding the cache-wide lock.
    /// Eviction only drops the cache's reference; an active read keeps its slot.
    pub(super) fn read_range(
        &self,
        runtime: &Runtime,
        reader: &Reader,
        id: u64,
        node: &FilesystemNode,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>> {
        let ContentKey::Regular { digest, .. } = node.content_key() else {
            anyhow::bail!("not a regular file")
        };
        if offset >= node.size() || length == 0 {
            return Ok(Vec::new());
        }
        let _entered = runtime.enter();
        let counters = &reader.counters;
        let slot = self.slot(id, counters);
        let lock_started = self.trace_reads.then(Instant::now);
        let mut parked = slot.lock().unwrap();
        if let Some(started) = lock_started {
            counters
                .reader_lock_ns
                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        // Take ownership before any fallible work. Errors drop the stream and
        // leave the slot empty, so callers cannot reuse a damaged/unparked reader.
        let mut stream = match parked.take() {
            Some(stream) => stream,
            None => runtime
                .block_on(reader.open_stream(&casita::BlobId::new(digest)))?
                .ok_or_else(|| anyhow!("blob missing from casita: {digest}"))?,
        };
        let wanted = (node.size() - offset).min(u64::from(length)) as usize;
        let bytes = runtime.block_on(async {
            let seek_started = self.trace_reads.then(Instant::now);
            stream.seek(io::SeekFrom::Start(offset)).await?;
            if let Some(started) = seek_started {
                counters
                    .seek_ns
                    .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            }
            let read_started = self.trace_reads.then(Instant::now);
            let mut bytes = vec![0; wanted];
            stream.read_exact(&mut bytes).await?;
            if let Some(started) = read_started {
                counters
                    .stream_read_ns
                    .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            }
            // Idle readers must release shared prefetch reservations before
            // another file needs them. No successful path can skip parking.
            stream.park().await;
            Ok::<_, io::Error>(bytes)
        })?;
        *parked = Some(stream);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{import, Backend};

    #[test]
    fn failed_range_is_dropped_and_retry_reopens() -> Result<()> {
        let base = tempfile::tempdir()?;
        let source = base.path().join("source");
        let repository = base.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join("data"), b"repository bytes")?;
        import(&source, &repository, "fixture", "snapshot.json")?;
        let backend = Backend::open(&repository)?;
        let entry = backend.lookup(4, b"data")?.unwrap();
        assert_eq!(backend.read(entry.id, 0, 4)?, b"repo");
        let opened = backend.reader.counters.opens.load(Ordering::Relaxed);

        let ContentKey::Regular { digest, .. } = entry.node.as_ref().unwrap().content_key() else {
            unreachable!()
        };
        // Advertise a range longer than the actual blob to fail read_exact after
        // the cached stream has been acquired and some bytes have been read.
        let oversized = FilesystemNode::from_casita(casita::Node::File {
            digest: casita::BlobId::new(digest),
            size: 32,
            executable: false,
        });
        let error = backend
            .file_readers
            .read_range(
                &backend.runtime,
                &backend.reader,
                entry.id,
                &oversized,
                0,
                32,
            )
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            backend.reader.counters.opens.load(Ordering::Relaxed),
            opened
        );
        assert_eq!(backend.read(entry.id, 5, 5)?, b"itory");
        assert_eq!(
            backend.reader.counters.opens.load(Ordering::Relaxed),
            opened + 1
        );
        assert_eq!(backend.read(entry.id, 0, 4)?, b"repo");
        assert_eq!(
            backend.reader.counters.opens.load(Ordering::Relaxed),
            opened + 1
        );
        backend.flush()?;
        Ok(())
    }
}
