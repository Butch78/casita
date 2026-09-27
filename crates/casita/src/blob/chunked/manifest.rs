//! Blob manifest wire format and bounded chunk decompression.

pub(super) use crate::wire::{decode_manifest, encode_manifest};

/// Zstd-decompress `compressed`, refusing output larger than `limit` bytes so
/// a malformed or hostile chunk cannot inflate without bound.
pub(crate) use crate::compression::decompress as decompress_capped;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::{ChunkMeta, MAX_CHUNK_SIZE};
    use crate::digest::{ChunkId, DIGEST_LEN};

    #[test]
    fn manifest_roundtrip_preserves_chunk_order_and_sizes() {
        let chunks = vec![
            ChunkMeta {
                digest: ChunkId::new([1; DIGEST_LEN].into()),
                size: 7,
            },
            ChunkMeta {
                digest: ChunkId::new([2; DIGEST_LEN].into()),
                size: 11,
            },
        ];
        assert_eq!(decode_manifest(&encode_manifest(&chunks)).unwrap(), chunks);
    }

    #[test]
    fn manifest_rejects_chunk_above_the_allocation_limit() {
        let bytes = encode_manifest(&[ChunkMeta {
            digest: ChunkId::new([3; DIGEST_LEN].into()),
            size: MAX_CHUNK_SIZE + 1,
        }]);
        assert!(decode_manifest(&bytes).is_err());
    }

    #[test]
    fn decompression_cap_accepts_exact_size_and_rejects_larger_output() {
        let data = vec![0xabu8; 128];
        let compressed = zstd::encode_all(&data[..], zstd::DEFAULT_COMPRESSION_LEVEL).unwrap();
        assert_eq!(decompress_capped(&compressed, data.len()).unwrap(), data);
        assert!(decompress_capped(&compressed, data.len() - 1).is_err());
    }

    #[test]
    fn chunk_decoder_preserves_legacy_frames_limits_and_error_recovery() {
        for size in [0, 1, 65535, 65536, 65537, 131072] {
            let bytes: Vec<_> = (0..size).map(|i| (i * 31) as u8).collect();
            let sized = crate::compression::compress(&bytes, 1).unwrap();
            let unsized_frame = zstd::encode_all(bytes.as_slice(), 1).unwrap();
            let split = size / 2;
            let mut concatenated = crate::compression::compress(&bytes[..split], 1).unwrap();
            concatenated.extend(crate::compression::compress(&bytes[split..], 1).unwrap());
            for frame in [&sized, &unsized_frame, &concatenated] {
                assert_eq!(decompress_capped(frame, size).unwrap(), bytes);
                assert_eq!(decompress_capped(frame, size + 1).unwrap(), bytes);
                if size > 0 {
                    assert!(decompress_capped(frame, size - 1).is_err());
                }
                assert!(decompress_capped(&frame[..frame.len() - 1], size).is_err());
                let mut trailing = frame.to_vec();
                trailing.extend_from_slice(b"not a frame");
                assert!(decompress_capped(&trailing, size).is_err());
                // An error must not poison the cached decoder for the next chunk.
                assert_eq!(decompress_capped(&sized, size).unwrap(), bytes);
            }
        }
        assert!(decompress_capped(b"invalid", 1024).is_err());
    }
}
