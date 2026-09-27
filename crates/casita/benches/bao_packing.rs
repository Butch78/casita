//! Feasibility probe only: durable loose Bao objects versus indexed sidecar packs.
//! This does not change Casita's production storage format.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use casita::BlobId;
use casita::experimental::verified::{build_outboard, decode_slice, encode_slice};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::{StreamExt, TryStreamExt};
use object_store::{ObjectStoreExt, local::LocalFileSystem, path::Path};

const TARGET: usize = 64 * 1024;
const ROW: usize = 80; // blob digest, pack digest, offset, length
const FOOTER_ROW: usize = 48; // blob digest, offset, length
const MAGIC: &[u8; 8] = b"baoprobe";

struct Fixture {
    payload: Bytes,
    digest: BlobId,
    outboard: Bytes,
}

fn loose_path(digest: &BlobId) -> Path {
    let hex = digest.as_digest().to_hex();
    Path::from(format!("bao/b3/{}/{hex}", &hex[..2]))
}

async fn write_loose(store: &LocalFileSystem, fixtures: &[Fixture]) {
    futures::stream::iter(fixtures.iter().map(|f| async move {
        store
            .put(&loose_path(&f.digest), f.outboard.clone().into())
            .await
            .map(|_| ())
    }))
    .buffer_unordered(16)
    .try_collect::<Vec<_>>()
    .await
    .unwrap();
}

async fn write_packed(store: &LocalFileSystem, fixtures: &[Fixture]) {
    let mut index = Vec::with_capacity(fixtures.len() * ROW);
    let mut begin = 0;
    while begin < fixtures.len() {
        let mut payload = Vec::new();
        let mut footer = Vec::new();
        let mut end = begin;
        while end < fixtures.len()
            && (payload.is_empty()
                || payload.len() + footer.len() + fixtures[end].outboard.len() + FOOTER_ROW + 16
                    <= TARGET)
        {
            let fixture = &fixtures[end];
            footer.extend_from_slice(fixture.digest.as_digest().as_bytes());
            footer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            footer.extend_from_slice(&(fixture.outboard.len() as u64).to_le_bytes());
            payload.extend_from_slice(&fixture.outboard);
            end += 1;
        }
        payload.extend_from_slice(&footer);
        payload.extend_from_slice(&(footer.len() as u64).to_le_bytes());
        payload.extend_from_slice(MAGIC);
        let digest = blake3::hash(&payload);
        store
            .put(
                &Path::from(format!("bao-packs/{}", digest.to_hex())),
                payload.into(),
            )
            .await
            .unwrap();
        for row in footer.chunks_exact(FOOTER_ROW) {
            index.extend_from_slice(&row[..32]);
            index.extend_from_slice(digest.as_bytes());
            index.extend_from_slice(&row[32..]);
        }
        begin = end;
    }
    // Include the durable lookup index, not just pack writes, in the timer.
    store
        .put(&Path::from("bao-index"), index.into())
        .await
        .unwrap();
}

fn integer(bytes: &[u8]) -> usize {
    usize::try_from(u64::from_le_bytes(bytes.try_into().unwrap())).unwrap()
}

async fn verify(store: &LocalFileSystem, fixtures: &[Fixture], packed: bool) {
    let mut locations = BTreeMap::new();
    let mut packs = BTreeMap::new();
    if packed {
        let index = store
            .get(&Path::from("bao-index"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(index.len(), fixtures.len() * ROW);
        for row in index.chunks_exact(ROW) {
            let blob: [u8; 32] = row[..32].try_into().unwrap();
            let pack: [u8; 32] = row[32..64].try_into().unwrap();
            assert!(
                locations
                    .insert(blob, (pack, integer(&row[64..72]), integer(&row[72..80])))
                    .is_none()
            );
            if let std::collections::btree_map::Entry::Vacant(entry) = packs.entry(pack) {
                let path = Path::from(format!("bao-packs/{}", blake3::Hash::from(pack).to_hex()));
                let bytes = store.get(&path).await.unwrap().bytes().await.unwrap();
                assert_eq!(blake3::hash(&bytes).as_bytes(), &pack);
                assert_eq!(&bytes[bytes.len() - 8..], MAGIC);
                let footer_len = integer(&bytes[bytes.len() - 16..bytes.len() - 8]);
                assert_eq!(footer_len % FOOTER_ROW, 0);
                let start = bytes.len().checked_sub(16 + footer_len).unwrap();
                for item in bytes[start..bytes.len() - 16].chunks_exact(FOOTER_ROW) {
                    let offset = integer(&item[32..40]);
                    let len = integer(&item[40..48]);
                    assert!(offset.checked_add(len).unwrap() <= start);
                }
                entry.insert(bytes);
            }
        }
    }
    for fixture in fixtures {
        let outboard = if packed {
            let (pack, offset, len) = locations[fixture.digest.as_digest().as_bytes()];
            let bytes = &packs[&pack];
            let footer_len = integer(&bytes[bytes.len() - 16..bytes.len() - 8]);
            let footer_start = bytes.len() - 16 - footer_len;
            assert!(
                bytes[footer_start..bytes.len() - 16]
                    .chunks_exact(FOOTER_ROW)
                    .any(|row| &row[..32] == fixture.digest.as_digest().as_bytes()
                        && integer(&row[32..40]) == offset
                        && integer(&row[40..48]) == len)
            );
            bytes.slice(offset..offset + len)
        } else {
            store
                .get(&loose_path(&fixture.digest))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        };
        assert_eq!(outboard, fixture.outboard);
        let size = fixture.payload.len() as u64;
        let offset = size / 2;
        let len = (size - offset).min(8192);
        let proof = encode_slice(
            fixture.payload.clone(),
            outboard,
            &fixture.digest,
            size,
            offset,
            len,
        )
        .await
        .unwrap();
        let decoded = decode_slice(&proof, &fixture.digest, size, offset, len)
            .await
            .unwrap();
        assert_eq!(
            decoded.as_ref(),
            &fixture.payload[offset as usize..(offset + len) as usize]
        );
    }
}

fn benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("bao_packing");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(1));
    for (count, size) in [
        (1, 16385),
        (32, 16385),
        (584, 16385),
        (585, 16385),
        (586, 16385),
        (16, 1064959),
        (16, 1064960),
        (16, 1064961),
    ] {
        let fixtures = runtime.block_on(async {
            let mut fixtures = Vec::new();
            for index in 0..count {
                let seed = blake3::hash(&(index as u64).to_le_bytes());
                let payload = Bytes::from(
                    (0..size)
                        .map(|offset| seed.as_bytes()[offset % 32])
                        .collect::<Vec<_>>(),
                );
                let (outboard, digest) = build_outboard(payload.clone()).await.unwrap();
                assert_eq!(
                    digest.as_digest().as_bytes(),
                    blake3::hash(&payload).as_bytes()
                );
                fixtures.push(Fixture {
                    payload,
                    digest,
                    outboard,
                });
            }
            fixtures
        });
        for packed in [false, true] {
            let mode = if packed { "packed" } else { "loose" };
            group.bench_function(BenchmarkId::new(format!("{mode}-{size}"), count), |b| {
                b.iter_custom(|iterations| {
                    runtime.block_on(async {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            let directory = tempfile::tempdir().unwrap();
                            let store = LocalFileSystem::new_with_prefix(directory.path())
                                .unwrap()
                                .with_fsync(true);
                            let start = Instant::now();
                            if packed {
                                write_packed(&store, &fixtures).await;
                            } else {
                                write_loose(&store, &fixtures).await;
                            }
                            elapsed += start.elapsed();
                            drop(store);
                            let reopened =
                                LocalFileSystem::new_with_prefix(directory.path()).unwrap();
                            verify(&reopened, &fixtures, packed).await;
                        }
                        elapsed
                    })
                });
            });
        }
    }
    group.finish();
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
