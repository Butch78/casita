#![no_main]

use std::pin::Pin;
use std::task::{Context, Poll};

use casita::experimental::{
    CasitarFrameHeader, CasitarHeader, CasitarReadFrame, CasitarReader, CasitarStreamLimits,
};
use libfuzzer_sys::fuzz_target;
use tokio::io::{AsyncRead, ReadBuf};

const MAX_INPUT: usize = 256 * 1024;

struct Fragmented<'a> {
    input: &'a [u8],
    position: usize,
    fragment: usize,
}

impl AsyncRead for Fragmented<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let available = &self.input[self.position..];
        let take = available.len().min(buffer.remaining()).min(self.fragment);
        buffer.put_slice(&available[..take]);
        self.position += take;
        Poll::Ready(Ok(()))
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Some((&fragment, archive)) = data.split_first() else {
        return;
    };

    let _ = CasitarHeader::decode_prefix(archive);
    let _ = CasitarFrameHeader::decode_prefix(archive);
    for end in 0..archive.len().min(128) {
        let _ = CasitarHeader::decode_prefix(&archive[..end]);
        let _ = CasitarFrameHeader::decode_prefix(&archive[..end]);
    }

    let reader = Fragmented {
        input: archive,
        position: 0,
        fragment: usize::from(fragment).max(1),
    };
    let limits = CasitarStreamLimits {
        max_header_bytes: 64 * 1024,
        max_record_bytes: 64 * 1024,
        max_payload_bytes: 128 * 1024,
        max_total_payload_bytes: 192 * 1024,
        max_archive_bytes: MAX_INPUT as u64,
        max_payloads: 512,
        max_records: 512,
        read_buffer_bytes: 4 * 1024,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread Tokio runtime");
    runtime.block_on(async move {
        let Ok(mut reader) = CasitarReader::open(reader, limits).await else {
            return;
        };
        loop {
            match reader.next_frame().await {
                Ok(Some(CasitarReadFrame::Payload { .. })) => {
                    if reader
                        .read_payload_to(&mut tokio::io::sink())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(Some(CasitarReadFrame::Record(_))) => {}
                Ok(None) | Err(_) => return,
            }
        }
    });
});
