//! A shaped in-process link: two duplex pairs joined by a relay that delays
//! every byte by half the round trip and paces each direction at a byte
//! rate, so a transfer can be measured as if it crossed a slow network. The
//! relay also counts the bytes carried in each direction.
//!
//! Shared by benchmarks and examples through `#[path]`, so it depends on
//! nothing but Tokio.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};

/// The two ends of a shaped link plus its byte counters.
pub struct ShapedLink {
    /// The client's end.
    pub client: DuplexStream,
    /// The server's end.
    pub server: DuplexStream,
    /// Bytes the client wrote toward the server.
    pub client_to_server: Arc<AtomicU64>,
    /// Bytes the server wrote toward the client.
    pub server_to_client: Arc<AtomicU64>,
    /// The relay task; finishes when both ends close.
    pub relay: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// Build a link with the given round trip and per-direction byte rate; zero
/// bytes per second means unpaced. The relay must be polled, so create it on
/// a running runtime.
pub fn shaped_link(rtt: Duration, bytes_per_second: u64) -> ShapedLink {
    let (client, relay_client) = tokio::io::duplex(8 * 1024 * 1024);
    let (relay_server, server) = tokio::io::duplex(8 * 1024 * 1024);
    let (relay_client_read, relay_client_write) = tokio::io::split(relay_client);
    let (relay_server_read, relay_server_write) = tokio::io::split(relay_server);
    let client_to_server = Arc::new(AtomicU64::new(0));
    let server_to_client = Arc::new(AtomicU64::new(0));
    let one_way = rtt / 2;
    let up = client_to_server.clone();
    let down = server_to_client.clone();
    let relay = tokio::spawn(async move {
        tokio::try_join!(
            delayed_direction(
                relay_client_read,
                relay_server_write,
                one_way,
                bytes_per_second,
                up
            ),
            delayed_direction(
                relay_server_read,
                relay_client_write,
                one_way,
                bytes_per_second,
                down
            ),
        )?;
        Ok(())
    });
    ShapedLink {
        client,
        server,
        client_to_server,
        server_to_client,
        relay,
    }
}

async fn delayed_direction<R, W>(
    mut reader: R,
    mut writer: W,
    one_way_delay: Duration,
    bytes_per_second: u64,
    counter: Arc<AtomicU64>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<(tokio::time::Instant, Vec<u8>)>(64);
    let read = async move {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            counter.fetch_add(read as u64, Ordering::Relaxed);
            let payload = buffer[..read].to_vec();
            sender
                .send((tokio::time::Instant::now() + one_way_delay, payload))
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "relay writer stopped")
                })?;
        }
        Ok::<(), std::io::Error>(())
    };
    let write = async move {
        let mut next_send = tokio::time::Instant::now();
        while let Some((ready_at, payload)) = receiver.recv().await {
            let ready_at = if bytes_per_second == 0 {
                ready_at
            } else {
                next_send = next_send.max(ready_at)
                    + Duration::from_secs_f64(payload.len() as f64 / bytes_per_second as f64);
                next_send
            };
            tokio::time::sleep_until(ready_at).await;
            writer.write_all(&payload).await?;
            writer.flush().await?;
        }
        writer.shutdown().await
    };
    tokio::try_join!(read, write)?;
    Ok(())
}
