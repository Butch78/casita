//! Bounded, cancellation-safe Bao streams. Producers are polled by the reader;
//! dropping a reader drops all pending I/O before releasing its retention pin.

use bao_tree::io::{fsm::decode_ranges, outboard::EmptyOutboard};
use bao_tree::{BaoTree, ChunkRanges};
use iroh_io::TokioStreamReader;
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};

use crate::{BlobId, blob::BlobStreamReader};

struct ProducedReader {
    reader: tokio::io::DuplexStream,
    producer: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
    failed: bool,
}

impl AsyncRead for ProducedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.failed {
            return Poll::Ready(Err(io::Error::other("verified stream failed")));
        }
        if let Some(producer) = &mut self.producer
            && let Poll::Ready(result) = producer.as_mut().poll(cx)
        {
            self.producer = None;
            if let Err(error) = result {
                self.failed = true;
                return Poll::Ready(Err(error));
            }
        }
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

pub(crate) fn produced<F, Fut>(make: F) -> Box<dyn BlobStreamReader>
where
    F: FnOnce(tokio::io::DuplexStream) -> Fut,
    Fut: Future<Output = io::Result<()>> + Send + 'static,
{
    let (reader, writer) = tokio::io::duplex(32 * 1024);
    Box::new(ProducedReader {
        reader,
        producer: Some(Box::pin(make(writer))),
        failed: false,
    })
}

/// Decode a complete Bao stream, releasing only authenticated plaintext.
/// The size is an untrusted decoding hint: successful EOF authenticates it.
pub fn decode<R: AsyncRead + Send + Unpin + 'static>(
    reader: R,
    digest: BlobId,
    size: u64,
) -> Box<dyn BlobStreamReader> {
    produced(move |writer| async move {
        let mut input = TokioStreamReader(reader);
        let mut outboard = EmptyOutboard {
            tree: BaoTree::new(size, super::BLOCK_SIZE),
            root: super::to_hash(&digest),
        };
        let mut output = SequentialWriter {
            writer,
            position: 0,
            size,
        };
        decode_ranges(&mut input, ChunkRanges::all(), &mut output, &mut outboard)
            .await
            .map_err(io::Error::other)?;
        if output.position != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "verified stream length mismatch",
            ));
        }
        let mut tail = [0];
        if input.0.read(&mut tail).await? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing bytes in Bao stream",
            ));
        }
        Ok(())
    })
}

struct SequentialWriter {
    writer: tokio::io::DuplexStream,
    position: u64,
    size: u64,
}

impl iroh_io::AsyncSliceWriter for SequentialWriter {
    async fn write_bytes_at(&mut self, offset: u64, bytes: bytes::Bytes) -> io::Result<()> {
        self.write_at(offset, &bytes).await
    }
    async fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if offset != self.position || bytes.len() as u64 > self.size.saturating_sub(offset) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "nonsequential verified output",
            ));
        }
        self.writer.write_all(bytes).await?;
        self.position += bytes.len() as u64;
        Ok(())
    }
    async fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
    async fn set_len(&mut self, _: u64) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::{BlobStore, MemoryBlobStore};

    #[tokio::test]
    async fn proof_stream_roundtrip_and_no_unauthenticated_output() {
        for size in [0, 1, 1024, 16383, 16384, 16385, 32768, 32769, 131073] {
            let bytes = (0..size).map(|i| (i * 31) as u8).collect::<Vec<_>>();
            let store = MemoryBlobStore::new();
            let digest = store.put_slice(&bytes).await.unwrap();
            let mut proof = store
                .open_proof(&digest, size as u64)
                .await
                .unwrap()
                .unwrap();
            let mut encoded = Vec::new();
            proof.read_to_end(&mut encoded).await.unwrap();
            assert_eq!(
                encoded.len() as u64,
                size as u64 + BaoTree::new(size as u64, super::super::BLOCK_SIZE).outboard_size()
            );
            let mut reader = decode(std::io::Cursor::new(encoded.clone()), digest, size as u64);
            let mut output = Vec::new();
            reader.read_to_end(&mut output).await.unwrap();
            assert_eq!(output, bytes);
            if !encoded.is_empty() {
                for at in [0, encoded.len() / 2, encoded.len() - 1] {
                    let mut bad = encoded.clone();
                    bad[at] ^= 1;
                    let mut reader = decode(std::io::Cursor::new(bad), digest, size as u64);
                    let mut output = Vec::new();
                    assert!(reader.read_to_end(&mut output).await.is_err());
                    assert_eq!(output, bytes[..output.len()]);
                    assert!(reader.read(&mut [0; 1]).await.is_err());
                }
                let mut reader = decode(
                    std::io::Cursor::new(encoded[..encoded.len() - 1].to_vec()),
                    digest,
                    size as u64,
                );
                assert!(reader.read_to_end(&mut Vec::new()).await.is_err());
            }
            for hint in [0, size as u64 + 1] {
                if hint == size as u64 {
                    continue;
                }
                let mut reader = decode(std::io::Cursor::new(encoded.clone()), digest, hint);
                let mut output = Vec::new();
                assert!(reader.read_to_end(&mut output).await.is_err());
                assert_eq!(output, bytes[..output.len()]);
            }
            let mut trailing = encoded;
            trailing.push(0);
            assert!(
                decode(std::io::Cursor::new(trailing), digest, size as u64)
                    .read_to_end(&mut Vec::new())
                    .await
                    .is_err()
            );
        }
    }
}
