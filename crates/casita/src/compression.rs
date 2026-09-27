//! Reusable codecs for independent, bounded storage frames.
//!
//! Calls run synchronously on the caller's blocking worker. Contexts stay on
//! that worker rather than crossing an await or contending on a global mutex.

use std::cell::RefCell;
use std::io::{self, Read};

thread_local! {
    static COMPRESSOR: RefCell<Option<(i32, zstd::bulk::Compressor<'static>)>> = const { RefCell::new(None) };
    static DECOMPRESSOR: RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { RefCell::new(None) };
}

pub(crate) fn compress(bytes: &[u8], level: i32) -> io::Result<Vec<u8>> {
    let mut encoded = COMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((previous, compressor)) = slot.as_mut() {
            if *previous != level {
                compressor.set_compression_level(level)?;
                *previous = level;
            }
            compressor.compress(bytes)
        } else {
            let mut compressor = zstd::bulk::Compressor::new(level)?;
            let encoded = compressor.compress(bytes)?;
            *slot = Some((level, compressor));
            Ok(encoded)
        }
    })?;
    // zstd reserves compress_bound(input.len()). Packs account for encoded
    // bytes, so retaining that spare capacity until publication would make
    // their memory grow with plaintext size for highly compressible inputs.
    encoded.shrink_to_fit();
    Ok(encoded)
}

pub(crate) fn decompress(bytes: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    // The convenience bulk API reserves its entire capacity argument unless
    // zstd's experimental feature is enabled. A corruption limit of 256 MiB
    // must not become a 256 MiB allocation for every small state record.
    let frame_size = zstd::zstd_safe::get_frame_content_size(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid zstd frame header"))?;
    if let Some(size) = frame_size {
        let size = usize::try_from(size)
            .ok()
            .filter(|size| *size <= limit)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "decoded frame exceeds limit")
            })?;
        let compressed_size =
            zstd::zstd_safe::find_frame_compressed_size(bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    zstd::zstd_safe::get_error_name(error),
                )
            })?;
        if compressed_size == bytes.len() {
            return DECOMPRESSOR.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(zstd::bulk::Decompressor::new()?);
                }
                slot.as_mut()
                    .expect("initialized decoder")
                    .decompress(bytes, size)
            });
        }
    }

    // Older frames may omit their size or concatenate multiple frames. Keep
    // their bounded decoding behavior without reserving the maximum up front.
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut decoded = Vec::new();
    decoder
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut decoded)?;
    if decoded.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "decoded frame exceeds limit",
        ));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_frames_do_not_retain_plaintext_sized_allocations() {
        for size in [0, 1, 1024, 65536, 524288] {
            for compressible in [false, true] {
                let mut body = vec![42; size];
                if !compressible {
                    blake3::Hasher::new()
                        .update(b"bounded compression allocation")
                        .finalize_xof()
                        .fill(&mut body);
                }
                let encoded = compress(&body, 3).unwrap();
                assert!(encoded.capacity() <= encoded.len().max(1) * 2);
                assert_eq!(decompress(&encoded, body.len()).unwrap(), body);
            }
        }
    }

    #[test]
    fn bounded_decoding_covers_sized_unsized_and_concatenated_frames() {
        let body = b"a bounded storage record";
        let sized = compress(body, 1).unwrap();
        let unknown_size = zstd::stream::encode_all(body.as_slice(), 1).unwrap();
        for frame in [&sized, &unknown_size] {
            let decoded = decompress(frame, 256 * 1024 * 1024).unwrap();
            assert_eq!(decoded, body);
            assert!(decoded.capacity() < 1024);
            assert!(decompress(frame, body.len() - 1).is_err());
            assert!(decompress(&frame[..frame.len() - 1], body.len()).is_err());
        }
        let mut concatenated = sized.clone();
        concatenated.extend_from_slice(&sized);
        assert_eq!(
            decompress(&concatenated, 2 * body.len()).unwrap(),
            body.repeat(2)
        );
        assert!(decompress(&concatenated, 2 * body.len() - 1).is_err());
        assert!(decompress(b"invalid", 1024).is_err());
        assert_eq!(decompress(&compress(b"", 1).unwrap(), 0).unwrap(), b"");
    }

    #[test]
    fn changing_compression_levels_preserves_independent_frames() {
        for level in [1, 3, 1, 3] {
            let body = b"codec contexts may serve metadata and chunks on the same worker";
            assert_eq!(
                decompress(&compress(body, level).unwrap(), body.len()).unwrap(),
                body
            );
        }
    }
}
