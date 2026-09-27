//! Bounded CPU handoffs; neither reads nor backpressure occupy blocking threads.
use std::io;
use std::sync::Arc;

use flate2::{Compress, Compression, FlushCompress, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::Semaphore;

use super::{GitFetchError, ObjectKey, PackWriter, RepositoryError, SIDEBAND_DATA};

const INPUT_BYTES: usize = 1024 * 1024;
const READ_BYTES: usize = 64 * 1024;
const READS_PER_BATCH: usize = INPUT_BYTES / READ_BYTES;

struct Encoder {
    codec: Compress,
    input: Vec<u8>,
    consumed: usize,
    output: Vec<u8>,
    produced: usize,
    finished: bool,
}

impl Encoder {
    fn new(level: u32, batch_bytes: usize) -> Self {
        Self {
            codec: Compress::new(Compression::new(level), true),
            input: Vec::new(),
            consumed: 0,
            // Room for this batch and pending codec output. The fixed capacity
            // remains a hard bound even if a backend needs another step.
            output: vec![0; batch_bytes + SIDEBAND_DATA],
            produced: 0,
            finished: false,
        }
    }

    async fn step(mut self, finish: bool, permits: &Arc<Semaphore>) -> io::Result<Self> {
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .expect("fetch encoder admission is never closed");
        let mut job = BlockingJob(tokio::task::spawn_blocking(move || {
            // Cancellation cannot stop an executing codec call. Keep admission
            // until that bounded call finishes, even if its caller disappears.
            let _permit = permit;
            self.produced = 0;
            loop {
                let before_in = self.codec.total_in();
                let before_out = self.codec.total_out();
                let status = self
                    .codec
                    .compress(
                        &self.input[self.consumed..],
                        &mut self.output[self.produced..],
                        if finish {
                            FlushCompress::Finish
                        } else {
                            FlushCompress::None
                        },
                    )
                    .map_err(io::Error::other)?;
                let consumed = (self.codec.total_in() - before_in) as usize;
                let produced = (self.codec.total_out() - before_out) as usize;
                self.consumed += consumed;
                self.produced += produced;
                self.finished = status == Status::StreamEnd;
                if consumed == 0 && produced == 0 && !self.finished {
                    return Err(io::Error::other("zlib encoder made no forward progress"));
                }
                if self.finished
                    || self.produced == self.output.len()
                    || (!finish && self.consumed == self.input.len())
                {
                    break;
                }
            }
            Ok(self)
        }));
        (&mut job.0).await.map_err(io::Error::other)?
    }
}

// Abort queued jobs on cancellation. Running jobs own only bounded codec state
// and their admission permit; no repository reader or retention hold escapes.
struct BlockingJob<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for BlockingJob<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn write_payload<R, W>(
    reader: &mut R,
    key: &ObjectKey,
    size: u64,
    read_buffer_bytes: usize,
    level: u32,
    permits: &Arc<Semaphore>,
    pack: &mut PackWriter<'_, W>,
) -> Result<(), GitFetchError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let read_bytes = read_buffer_bytes.clamp(1, READ_BYTES);
    // Respect the caller's individual read bound and also bound the number of
    // immediately-ready reads, even with a one-byte read buffer.
    let batch_bytes = read_bytes * READS_PER_BATCH;
    let mut encoder = Encoder::new(level, batch_bytes);
    let mut remaining = size;
    loop {
        let wanted = remaining.min(batch_bytes as u64) as usize;
        encoder.input.resize(wanted, 0);
        let mut read = 0;
        let mut reads = 0;
        while read < wanted && reads < READS_PER_BATCH {
            let end = (read + read_bytes).min(wanted);
            let count = reader.read(&mut encoder.input[read..end]).await?;
            if count == 0 {
                return Err(
                    RepositoryError::Metadata(crate::MetadataError::Corruption(format!(
                        "Git object {key} ended with {} declared bytes remaining",
                        remaining - read as u64
                    )))
                    .into(),
                );
            }
            read += count;
            reads += 1;
        }
        encoder.input.truncate(read);
        encoder.consumed = 0;
        remaining -= read as u64;
        let finish = remaining == 0;
        if finish && reader.read(&mut [0u8; 1]).await? != 0 {
            return Err(
                RepositoryError::Metadata(crate::MetadataError::Corruption(format!(
                    "Git object {key} exceeds its declared payload size"
                )))
                .into(),
            );
        }
        loop {
            encoder = encoder.step(finish, permits).await?;
            for (index, chunk) in encoder.output[..encoder.produced]
                .chunks(SIDEBAND_DATA)
                .enumerate()
            {
                if index != 0 && index % 4 == 0 {
                    tokio::task::yield_now().await;
                }
                pack.write_hashed(chunk).await?;
            }
            if encoder.finished || (!finish && encoder.consumed == read) {
                break;
            }
        }
        if encoder.finished {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
