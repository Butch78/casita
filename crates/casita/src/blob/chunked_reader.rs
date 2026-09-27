//! [`ChunkedReader`]: a seekable [`BlobReader`] that assembles a blob from its
//! chunks on demand.
//!
//! Given the ordered `(digest, size)` chunk list of a blob, it lazily fetches
//! each chunk from a [`ChunkSource`] as it is read (prefetching one chunk
//! ahead), and supports seeking by re-opening the read stream at the chunk
//! containing the target offset (skipping within that chunk).
//!
//! Chunks are fetched through [`ChunkSource`], not the public [`BlobStore`]
//! surface: a chunk is an internal storage detail of some blob, never a blob in
//! its own right. When a whole-blob digest is supplied, a straight sequential
//! read (from the start, no seeking) is verified against it at EOF, so a
//! tampered manifest that lists the wrong chunks is rejected — the same
//! integrity guarantee the per-chunk digest check gives, lifted to the whole
//! blob.

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::StreamExt;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use tokio_util::io::StreamReader;

use super::{BlobIntegrityError, BlobReader};
use crate::digest::{BlobId, ChunkId};

/// How many chunks to fetch concurrently while reading sequentially, so the
/// next chunk downloads while the current one is being consumed.
const CHUNK_READAHEAD: usize = 2;

// Retain verified bytes after repeated nonsequential seeks or when parking a
// partial chunk. Uninterrupted sequential streams do not populate this cache.
// This is per reader, in addition to in-flight data, with a process-wide cap.
pub(crate) const DECODED_CACHE_BYTES: usize = 2 * 1024 * 1024;
const DECODED_CACHE_ENTRIES: usize = 64;

pub(crate) const SHARED_DECODED_CACHE_BYTES: usize = 32 * 1024 * 1024;

struct CacheBudget {
    used: AtomicUsize,
    capacity: usize,
}
impl CacheBudget {
    fn shared() -> Arc<Self> {
        static BUDGET: OnceLock<Arc<CacheBudget>> = OnceLock::new();
        BUDGET
            .get_or_init(|| {
                Arc::new(Self {
                    used: AtomicUsize::new(0),
                    capacity: SHARED_DECODED_CACHE_BYTES,
                })
            })
            .clone()
    }
    fn copy(self: &Arc<Self>, bytes: &[u8]) -> Option<Bytes> {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes.len())
                    .filter(|total| *total <= self.capacity)
            })
            .ok()?;
        let owner = CachedBytes {
            data: bytes.to_vec(),
            budget: self.clone(),
        };
        Some(Bytes::from_owner(owner))
    }
}
struct CachedBytes {
    data: Vec<u8>,
    budget: Arc<CacheBudget>,
}
impl AsRef<[u8]> for CachedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl Drop for CachedBytes {
    fn drop(&mut self) {
        self.budget
            .used
            .fetch_sub(self.data.len(), Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) fn shared_decoded_cache_usage() -> usize {
    CacheBudget::shared().used.load(Ordering::Relaxed)
}

#[derive(Default)]
struct MissedSeeks {
    targets: VecDeque<(ChunkId, u64)>,
    bytes: u64,
}

struct DecodedCache {
    active: AtomicBool,
    capacity: usize,
    entries: Mutex<VecDeque<(ChunkId, u64, Bytes)>>,
    budget: Arc<CacheBudget>,
    missed_seeks: Mutex<MissedSeeks>,
}
impl DecodedCache {
    fn observe_seek(&self, digest: ChunkId, size: u64) {
        if !self.active.load(Ordering::Relaxed) || size > self.capacity as u64 {
            return;
        }
        let mut missed = self.missed_seeks.lock().unwrap();
        let mut entries = self.entries.lock().unwrap();
        if entries.is_empty()
            || entries
                .iter()
                .any(|(id, len, _)| *id == digest && *len == size)
        {
            *missed = MissedSeeks::default();
        } else {
            missed.bytes = missed.bytes.saturating_add(size);
            let retained: u64 = entries.iter().map(|(_, size, _)| *size).sum();
            let repeated = missed.targets.contains(&(digest, size));
            if repeated && missed.bytes >= retained {
                // Demand must revisit an uncached target and account for at
                // least the retained set's size without a hit. Speculative
                // read-ahead never counts. This avoids resetting on a few
                // repeated misses at the end of a mostly cached cyclic scan.
                entries.clear();
                *missed = MissedSeeks::default();
            } else if !repeated {
                if missed.targets.len() == DECODED_CACHE_ENTRIES {
                    missed.targets.pop_front();
                }
                missed.targets.push_back((digest, size));
            }
        }
    }

    fn get(&self, digest: ChunkId, size: u64) -> Option<Bytes> {
        if !self.active.load(Ordering::Relaxed) || self.capacity == 0 {
            return None;
        }
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .find(|(id, len, _)| *id == digest && *len == size)
            .map(|entry| entry.2.clone())
    }
    fn insert(&self, digest: ChunkId, size: u64, bytes: &Bytes) {
        if !self.active.load(Ordering::Relaxed) || bytes.is_empty() || bytes.len() > self.capacity {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        if entries
            .iter()
            .any(|(id, len, _)| *id == digest && *len == size)
        {
            return;
        }
        let used: usize = entries.iter().map(|(_, _, bytes)| bytes.len()).sum();
        // Keep admitted chunks stable until demand demonstrates a phase change.
        // Replacing entries on each miss caused cyclic-scan decode thrashing.
        if used > self.capacity - bytes.len() || entries.len() >= DECODED_CACHE_ENTRIES {
            return;
        }
        // Copy without retaining a decode permit or physical read plan. The
        // nonblocking shared reservation stays attached to every Bytes clone,
        // including reads that outlive the cache. Exhaustion simply skips reuse.
        if let Some(bytes) = self.budget.copy(bytes) {
            entries.push_back((digest, size, bytes));
        }
    }
}

/// Fetches the verified bytes of a single stored chunk by digest.
///
/// This is the internal seam chunked blobs are reconstructed through. It is
/// deliberately *not* part of [`BlobStore`](super::BlobStore): a chunk is a
/// storage-internal fragment of some blob, not addressable content on its own.
#[async_trait]
pub(crate) trait ChunkSource: Send + Sync + 'static {
    /// Fetch one chunk's uncompressed bytes, verifying them against `digest`
    /// and `size`.
    async fn fetch_chunk(&self, digest: ChunkId, size: u64) -> io::Result<Bytes>;

    /// Release prefetch state once the reader has dropped pending chunk fetches.
    async fn park(&self) {}
}

/// Whole-blob integrity check for a straight sequential read. Present only while
/// the check is still live: a seek drops it (a partial or out-of-order read
/// cannot be checked against the whole-blob digest without the bao outboard),
/// and reaching EOF consumes it.
struct Verify {
    hasher: blake3::Hasher,
    expected: BlobId,
}

/// A reader that reconstructs a blob from its chunk list.
pub(crate) struct ChunkedReader {
    source: Arc<dyn ChunkSource>,
    /// `(start_offset, size, digest)` per chunk, ordered by offset.
    chunks: Vec<(u64, u64, ChunkId)>,
    total_len: u64,
    r: Box<dyn AsyncRead + Send + Unpin>,
    pos: u64,
    verify: Option<Verify>,
    decoded: Arc<DecodedCache>,
    seeks: u8,
}

#[async_trait]
impl BlobReader for ChunkedReader {
    async fn park(&mut self) {
        if let Some(index) = chunk_index_for(&self.chunks, self.pos) {
            let (start, size, digest) = self.chunks[index];
            if self.pos != start {
                // FSKit parks after small sequential reads. Reopening midway
                // through the same chunk would otherwise decode it on every
                // callback. Reuse the bounded verified-byte cache, never the
                // source's prefetch/decode reservations.
                self.decoded.active.store(true, Ordering::Relaxed);
                self.decoded.observe_seek(digest, size);
            }
        }
        // Drop pending chunk/decode futures before taking the source cursor lock.
        // Rebuilding is lazy and preserves position, verification and seek cache.
        self.r = reader_from_offset(
            &self.source,
            &self.chunks,
            self.total_len,
            self.pos,
            &self.decoded,
        );
        self.source.park().await;
    }
}

impl ChunkedReader {
    #[cfg(test)]
    pub(crate) fn decoded_cache_usage(&self) -> (usize, usize) {
        let entries = self.decoded.entries.lock().unwrap();
        (
            entries.iter().map(|entry| entry.2.len()).sum(),
            entries.len(),
        )
    }
    /// Build a reader over `chunks` (each `(digest, size)`), fetching chunk
    /// bytes from `source`.
    ///
    /// `whole_blob` enables verifying a straight, unseeked read against the
    /// blob's digest at EOF. Callers that only ever seek/slice (feeding
    /// bao-tree, which verifies independently) pass `None`.
    ///
    /// The chunk sizes must already be validated so their running offsets do
    /// not overflow `u64` (see `decode_manifest`).
    pub(crate) fn new(
        source: Arc<dyn ChunkSource>,
        chunks: impl IntoIterator<Item = (ChunkId, u64)>,
        whole_blob: Option<BlobId>,
    ) -> Self {
        Self::with_cache_capacity(source, chunks, whole_blob, DECODED_CACHE_BYTES)
    }

    pub(crate) fn with_cache_capacity(
        source: Arc<dyn ChunkSource>,
        chunks: impl IntoIterator<Item = (ChunkId, u64)>,
        whole_blob: Option<BlobId>,
        capacity: usize,
    ) -> Self {
        let mut offset = 0u64;
        let table: Vec<(u64, u64, ChunkId)> = chunks
            .into_iter()
            .map(|(digest, size)| {
                let start = offset;
                offset += size;
                (start, size, digest)
            })
            .collect();

        let decoded = Arc::new(DecodedCache {
            active: AtomicBool::new(false),
            capacity,
            entries: Mutex::default(),
            budget: CacheBudget::shared(),
            missed_seeks: Mutex::default(),
        });
        let r = reader_from_offset(&source, &table, offset, 0, &decoded);
        Self {
            source,
            chunks: table,
            total_len: offset,
            r,
            decoded,
            seeks: 0,
            pos: 0,
            verify: whole_blob.map(|expected| Verify {
                hasher: blake3::Hasher::new(),
                expected,
            }),
        }
    }
}

/// The index of the chunk containing `pos`, or `None` if out of range.
fn chunk_index_for(chunks: &[(u64, u64, ChunkId)], pos: u64) -> Option<usize> {
    chunks
        .binary_search_by(|(start, size, _)| {
            if pos < *start {
                std::cmp::Ordering::Greater
            } else if pos >= *start + *size {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .ok()
}

/// A fresh reader positioned at absolute `offset` within the blob.
fn reader_from_offset(
    source: &Arc<dyn ChunkSource>,
    chunks: &[(u64, u64, ChunkId)],
    total_len: u64,
    offset: u64,
    decoded: &Arc<DecodedCache>,
) -> Box<dyn AsyncRead + Send + Unpin> {
    if offset >= total_len {
        return Box::new(io::Cursor::new(Vec::<u8>::new()));
    }
    let idx = chunk_index_for(chunks, offset).expect("offset within range");
    let skip = offset - chunks[idx].0;
    let wanted: Vec<(ChunkId, u64)> = chunks[idx..].iter().map(|(_, s, d)| (*d, *s)).collect();
    let source = source.clone();
    let decoded = decoded.clone();

    let stream =
        tokio_stream::iter(wanted.into_iter().enumerate()).map(move |(i, (digest, size))| {
            let source = source.clone();
            let decoded = decoded.clone();
            async move {
                let bytes = if let Some(bytes) = decoded.get(digest, size) {
                    bytes
                } else {
                    let bytes = source.fetch_chunk(digest, size).await?;
                    if bytes.len() as u64 != size {
                        return Err(io::Error::other("decoded chunk size differs from manifest"));
                    }
                    decoded.insert(digest, size, &bytes);
                    bytes
                };
                // the first chunk starts mid-way when we sought into it.
                let bytes = if i == 0 && skip > 0 {
                    bytes.slice(skip as usize..)
                } else {
                    bytes
                };
                Ok::<Bytes, io::Error>(bytes)
            }
        });

    Box::new(StreamReader::new(Box::pin(
        stream.buffered(CHUNK_READAHEAD),
    )))
}

impl AsyncRead for ChunkedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // a genuine EOF is a zero-byte fill into a buffer that had room; a read
        // into a zero-capacity (or already-full) buffer also fills nothing but
        // is not EOF, and must not trip the whole-blob check below.
        let had_capacity = buf.remaining() > 0;
        let before = buf.filled().len();
        let res = Pin::new(&mut this.r).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &res {
            let new = &buf.filled()[before..];
            this.pos += new.len() as u64;
            if new.is_empty() && had_capacity {
                // EOF on a straight read: the assembled content must hash to the
                // blob digest, or the manifest is lying. The check runs once.
                if let Some(verify) = this.verify.take() {
                    if this.pos != this.total_len {
                        return Poll::Ready(Err(io::Error::other(BlobIntegrityError::Blob {
                            expected: verify.expected,
                        })));
                    }
                    let got = BlobId::new(verify.hasher.finalize().into());
                    if got != verify.expected {
                        return Poll::Ready(Err(io::Error::other(BlobIntegrityError::Blob {
                            expected: verify.expected,
                        })));
                    }
                }
            } else if let Some(verify) = this.verify.as_mut() {
                verify.hasher.update(new);
            }
        }
        res
    }
}

impl AsyncSeek for ChunkedReader {
    fn start_seek(self: Pin<&mut Self>, position: io::SeekFrom) -> io::Result<()> {
        let this = self.get_mut();
        let new_pos = match position {
            io::SeekFrom::Start(o) => o,
            io::SeekFrom::End(o) => checked_offset(this.total_len, o)?,
            io::SeekFrom::Current(o) => checked_offset(this.pos, o)?,
        };
        if new_pos > this.total_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek beyond end of blob",
            ));
        }
        // sequential callers seek to where they already are; keep the reader.
        if new_pos == this.pos {
            return Ok(());
        }
        // any real seek gives up the whole-blob check: we are no longer reading
        // the content start-to-end in order.
        this.verify = None;
        this.seeks = this.seeks.saturating_add(1);
        if this.seeks >= 2 {
            this.decoded.active.store(true, Ordering::Relaxed);
        }
        if let Some(index) = chunk_index_for(&this.chunks, new_pos) {
            let (_, size, digest) = this.chunks[index];
            this.decoded.observe_seek(digest, size);
        }
        this.pos = new_pos;
        this.r = reader_from_offset(
            &this.source,
            &this.chunks,
            this.total_len,
            new_pos,
            &this.decoded,
        );
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.pos))
    }
}

/// Add a signed offset to a base position, rejecting negative or overflowing
/// results.
fn checked_offset(base: u64, off: i64) -> io::Result<u64> {
    base.checked_add_signed(off)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek offset"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    struct Source {
        calls: AtomicUsize,
        bad: AtomicBool,
    }
    #[async_trait]
    impl ChunkSource for Source {
        async fn fetch_chunk(&self, digest: ChunkId, size: u64) -> io::Result<Bytes> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.bad.load(Ordering::Relaxed) {
                return Err(io::Error::other("corrupt chunk"));
            }
            let data = if digest == chunk(b"abcd") {
                b"abcd"
            } else {
                b"efgh"
            };
            assert_eq!(size, data.len() as u64);
            Ok(Bytes::copy_from_slice(data))
        }
    }
    fn chunk(data: &[u8]) -> ChunkId {
        ChunkId::new(blake3::hash(data).into())
    }
    fn source() -> Arc<Source> {
        Arc::new(Source {
            calls: AtomicUsize::new(0),
            bad: AtomicBool::new(false),
        })
    }
    fn reader(source: Arc<Source>, expected: &[u8]) -> ChunkedReader {
        ChunkedReader::with_cache_capacity(
            source,
            [(chunk(b"abcd"), 4), (chunk(b"efgh"), 4)],
            Some(BlobId::new(blake3::hash(expected).into())),
            8,
        )
    }
    #[tokio::test]
    async fn parked_sequential_reads_reuse_verified_chunks() {
        let source = source();
        let mut input = reader(source.clone(), b"abcdefgh");
        for expected in b"abcdefgh" {
            let mut byte = [0];
            input.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte[0], *expected);
            input.park().await;
        }
        assert_eq!(input.read(&mut [0]).await.unwrap(), 0);
        assert!(source.calls.load(Ordering::Relaxed) <= 4);
        assert!(input.decoded_cache_usage().0 <= 8);
    }

    #[tokio::test]
    async fn parking_preserves_position_and_whole_blob_verification() {
        for expected in [b"abcdefgh", b"wrong!!!"] {
            let mut input = reader(source(), expected);
            let mut prefix = [0; 3];
            input.read_exact(&mut prefix).await.unwrap();
            assert_eq!(&prefix, b"abc");
            input.park().await;
            input.park().await;
            let mut rest = Vec::new();
            let result = input.read_to_end(&mut rest).await;
            assert_eq!(result.is_ok(), expected == b"abcdefgh");
            assert_eq!(rest, b"defgh");
        }
    }

    #[tokio::test]
    async fn sequential_integrity_and_seek_reuse() {
        let source = source();
        let mut input = reader(source.clone(), b"abcdefgh");
        let mut output = Vec::new();
        input.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"abcdefgh");
        assert!(input.decoded.entries.lock().unwrap().is_empty());
        input.seek(io::SeekFrom::Start(1)).await.unwrap();
        output.clear();
        input.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"bcdefgh");
        assert!(input.decoded.entries.lock().unwrap().is_empty());
        input.seek(io::SeekFrom::Start(0)).await.unwrap();
        output.clear();
        input.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"abcdefgh");
        let calls = source.calls.load(Ordering::Relaxed);
        for offset in [0, 3, 4, 7, 8, 2] {
            input.seek(io::SeekFrom::Start(offset)).await.unwrap();
            let mut actual = [0; 3];
            let wanted = 3.min(8 - offset as usize);
            input.read_exact(&mut actual[..wanted]).await.unwrap();
            assert_eq!(
                &actual[..wanted],
                &b"abcdefgh"[offset as usize..offset as usize + wanted]
            );
        }
        assert_eq!(source.calls.load(Ordering::Relaxed), calls);
        assert!(input.seek(io::SeekFrom::Start(9)).await.is_err());
        let mut wrong = reader(source.clone(), b"wrong digest");
        assert!(wrong.read_to_end(&mut Vec::new()).await.is_err());
    }
    #[tokio::test]
    async fn failures_are_never_cached() {
        let source = source();
        source.bad.store(true, Ordering::Relaxed);
        let mut input = reader(source.clone(), b"abcdefgh");
        input.seek(io::SeekFrom::Start(2)).await.unwrap();
        input.seek(io::SeekFrom::Start(1)).await.unwrap();
        assert!(input.read_exact(&mut [0; 1]).await.is_err());
        assert!(input.decoded.entries.lock().unwrap().is_empty());
        source.bad.store(false, Ordering::Relaxed);
        input.seek(io::SeekFrom::Start(2)).await.unwrap();
        let mut byte = [0];
        input.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, b"c"[..]);
    }
    #[test]
    fn capacity_stable_admission_and_owner_release() {
        struct Owner {
            data: Vec<u8>,
            _held: Arc<()>,
        }
        impl AsRef<[u8]> for Owner {
            fn as_ref(&self) -> &[u8] {
                &self.data
            }
        }
        for capacity in [3, 4, 5] {
            let cache = DecodedCache {
                active: AtomicBool::new(true),
                capacity,
                entries: Mutex::default(),
                budget: CacheBudget::shared(),
                missed_seeks: Mutex::default(),
            };
            let owner = Arc::new(());
            let weak = Arc::downgrade(&owner);
            let bytes = Bytes::from_owner(Owner {
                data: b"abcd".to_vec(),
                _held: owner,
            });
            cache.insert(chunk(b"abcd"), 4, &bytes);
            drop(bytes);
            assert!(weak.upgrade().is_none(), "cache retained decode owner");
            assert_eq!(cache.get(chunk(b"abcd"), 4).is_some(), capacity >= 4);
            assert!(cache.get(chunk(b"abcd"), 3).is_none());
        }
        let cache = DecodedCache {
            active: AtomicBool::new(true),
            capacity: 8,
            entries: Mutex::default(),
            budget: CacheBudget::shared(),
            missed_seeks: Mutex::default(),
        };
        cache.insert(chunk(b"abcd"), 4, &Bytes::from_static(b"abcd"));
        cache.insert(chunk(b"efgh"), 4, &Bytes::from_static(b"efgh"));
        cache.get(chunk(b"abcd"), 4).unwrap();
        cache.insert(chunk(b"ijkl"), 4, &Bytes::from_static(b"ijkl"));
        assert!(cache.get(chunk(b"efgh"), 4).is_some());
        assert!(cache.get(chunk(b"ijkl"), 4).is_none());
        assert_eq!(
            cache
                .entries
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.2.len())
                .sum::<usize>(),
            8
        );
    }
    #[test]
    fn phase_change_requires_repeated_misses_without_a_hit() {
        let cache = DecodedCache {
            active: AtomicBool::new(true),
            capacity: 4,
            entries: Mutex::default(),
            budget: CacheBudget::shared(),
            missed_seeks: Mutex::default(),
        };
        cache.insert(chunk(b"abcd"), 4, &Bytes::from_static(b"abcd"));
        for _ in 0..4 {
            cache.observe_seek(chunk(b"efgh"), 4);
            cache.observe_seek(chunk(b"abcd"), 4);
        }
        assert!(cache.get(chunk(b"abcd"), 4).is_some());
        cache.observe_seek(chunk(b"efgh"), 4);
        assert!(cache.get(chunk(b"abcd"), 4).is_some());
        cache.observe_seek(chunk(b"efgh"), 4);
        assert!(cache.entries.lock().unwrap().is_empty());
        cache.insert(chunk(b"efgh"), 4, &Bytes::from_static(b"efgh"));
        assert!(cache.get(chunk(b"efgh"), 4).is_some());
    }

    #[test]
    fn shared_budget_counts_live_clones_and_recovers_after_release() {
        let budget = Arc::new(CacheBudget {
            used: AtomicUsize::new(0),
            capacity: 8,
        });
        let first = budget.copy(b"abcd").unwrap();
        let held = first.slice(1..);
        let second = budget.copy(b"efgh").unwrap();
        assert!(budget.copy(b"i").is_none());
        drop(first);
        assert_eq!(budget.used.load(Ordering::Relaxed), 8);
        drop(held);
        assert_eq!(budget.used.load(Ordering::Relaxed), 4);
        assert!(budget.copy(b"ijklm").is_none());
        let replacement = budget.copy(b"ijkl").unwrap();
        drop((second, replacement));
        assert_eq!(budget.used.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_budget_is_atomic_across_readers() {
        let budget = Arc::new(CacheBudget {
            used: AtomicUsize::new(0),
            capacity: 8,
        });
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let budget = budget.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let bytes = budget.copy(b"abcd");
                    barrier.wait();
                    barrier.wait();
                    bytes.is_some()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(budget.used.load(Ordering::Relaxed), 8);
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|thread| thread.join().unwrap().then_some(()))
                .count(),
            2
        );
        assert_eq!(budget.used.load(Ordering::Relaxed), 0);
    }
}
