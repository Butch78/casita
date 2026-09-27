//! Sliced payloads over the SSH stdio transport.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

use super::{SshTransferSession, serve_transfer_stdio};
use crate::blob::{BlobGc, BlobStore};
use crate::metadata::MetadataStore;
use crate::repository::Repository;
use crate::sync::{DestinationRoot, ObjectRequest, TransferReadSession, TransferRequest};
use crate::{ClosureStatus, MemoryBlobStore, MemoryMetadataStore, ObjectKey, RootChange, RootName};

fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut output = Vec::with_capacity(len + 8);
    while output.len() < len {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        output.extend_from_slice(&(value ^ (value >> 31)).to_le_bytes());
    }
    output.truncate(len);
    output
}

/// A rebuilt store path: the same bytes with a different 32-byte hash every
/// `spacing` bytes.
fn rebuilt(base: &[u8], spacing: usize) -> Vec<u8> {
    let mut bytes = base.to_vec();
    let mut position = spacing / 2;
    while position + 32 <= bytes.len() {
        for byte in &mut bytes[position..position + 32] {
            *byte = byte.wrapping_add(1);
        }
        position += spacing;
    }
    bytes
}

/// Counts the bytes a server writes onto the wire.
struct CountingWriter<W> {
    inner: W,
    bytes: Arc<AtomicU64>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for CountingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let written = Pin::new(&mut self.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(count)) = &written {
            self.bytes.fetch_add(*count as u64, Ordering::Relaxed);
        }
        written
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

type Memory = Repository<MemoryBlobStore, MemoryMetadataStore>;

fn repository() -> Memory {
    Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new().unwrap())
}

async fn publish_blob(repository: &Memory, name: &RootName, bytes: &[u8]) -> ObjectKey {
    let mutation = repository.mutation_session().await.unwrap();
    let staged = mutation.stage_blob(bytes).await.unwrap();
    let key = staged.record().key().clone();
    mutation
        .publish(
            vec![staged],
            vec![RootChange::Set {
                name: name.clone(),
                target: key.clone(),
            }],
        )
        .await
        .unwrap();
    key
}

/// Sync `target` under `name` from `source` into `destination` over a stdio
/// pair, returning the result and the bytes the server sent.
async fn sync(
    source: Memory,
    destination: &Memory,
    name: &RootName,
    target: &ObjectKey,
) -> (Sync, u64, Memory) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let bytes = Arc::new(AtomicU64::new(0));
    let counting = CountingWriter {
        inner: server_write,
        bytes: bytes.clone(),
    };
    let server = tokio::spawn(async move {
        let outcome = serve_transfer_stdio(&source, server_read, counting).await;
        (outcome, source)
    });
    let session = SshTransferSession::connect(Box::new(client_read), Box::new(client_write), None)
        .await
        .unwrap();
    let result = crate::transfer(
        &crate::sync::HeldSession(&session),
        destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: target.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: name.clone(),
                target: target.clone(),
            }],
        },
        crate::sync::TransferOptions::default(),
    )
    .await
    .unwrap();
    let operations = session.transport_operations().unwrap_or_default();
    drop(session);
    let (outcome, source) = server.await.unwrap();
    outcome.unwrap();
    (
        Sync {
            progress: result.progress,
            operations,
        },
        bytes.load(Ordering::Relaxed),
        source,
    )
}

/// One sync's progress and the requests it took.
struct Sync {
    progress: crate::sync::TransferProgress,
    operations: Vec<(String, u64)>,
}

/// A discovery answer carries payloads the receiver never asked for, which
/// pays only where this side can tell they are wanted. A receiver that offered
/// no bases holds nothing. One that offered bases and asks about a frontier of
/// many objects is walking a closure, and the batch pipeline behind that walk
/// asks its own store what it holds before requesting anything, which is what
/// this side cannot see.
#[tokio::test]
async fn a_discovery_answer_volunteers_payloads_only_where_they_are_wanted() {
    let source = repository();
    let mut keys = Vec::new();
    for index in 0..super::VOLUNTEER_MAX_REQUEST_KEYS + 1 {
        let name = RootName::try_from(format!("root{index}").as_str()).unwrap();
        keys.push(publish_blob(&source, &name, format!("payload {index}").as_bytes()).await);
    }
    let elsewhere = publish_blob(
        &source,
        &RootName::try_from("elsewhere").unwrap(),
        b"a blob the receiver already holds",
    )
    .await;
    let hold = source.retention_hold().await.unwrap();
    let none = std::collections::BTreeSet::new();
    let bases = std::collections::BTreeSet::from([elsewhere]);

    let answer = super::discover_closure(&hold, &none, &keys)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        answer.payloads.len(),
        keys.len(),
        "a receiver holding nothing wants every payload the answer found"
    );

    let answer = super::discover_closure(&hold, &bases, &keys)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.records.len(), keys.len());
    assert!(
        answer.payloads.is_empty(),
        "a frontier this wide is a closure walk, whose own pipeline knows better"
    );

    let answer = super::discover_closure(&hold, &bases, &keys[..1])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        answer.payloads.len(),
        1,
        "one object is a tree sync, and the answer is all the work it has"
    );
}

/// Slicing is not bounded by payload size. It was, at 64 MiB, while two
/// concurrent packed reads of blobs that large could wedge each other, and
/// that bound sent the largest paths of a closure as literals however little
/// of them had changed: 171 MB where slicing them costs 65 MB. A payload above
/// the old bound must arrive as copies.
#[tokio::test]
async fn a_payload_larger_than_the_retired_bound_is_still_sliced() {
    let name = RootName::try_from("large").unwrap();
    let base = pseudo_random(7, 65 * 1024 * 1024);
    let new = rebuilt(&base, 4 * 1024 * 1024);
    let source = repository();
    let base_key = publish_blob(&source, &name, &base).await;
    let destination = repository();
    let (_, _, source) = sync(source, &destination, &name, &base_key).await;

    let new_key = publish_blob(&source, &name, &new).await;
    let (second, second_bytes, _) = sync(source, &destination, &name, &new_key).await;
    assert!(
        second.progress.slice_copy_bytes >= new.len() as u64 * 95 / 100,
        "a payload above the retired bound arrived as literals: {:?}",
        second.progress
    );
    assert!(
        second_bytes * 50 < base.len() as u64,
        "wire {second_bytes} for a {} byte rebuild",
        new.len()
    );
}

/// A transfer round carries what one payload batch can hold, so a tree of many
/// small files costs a handful of requests rather than one per file. A round
/// rule that once stopped a round at the first record that did not fit turned
/// 76 paths into 2,527 requests, which is the shape this pins.
#[tokio::test]
async fn many_small_payloads_travel_in_batches_rather_than_one_request_each() {
    let name = RootName::try_from("tree").unwrap();
    let source = repository();
    let contents: Vec<Vec<u8>> = (0..150u64).map(|index| pseudo_random(index, 512)).collect();
    let names: Vec<String> = (0..150).map(|index| format!("file{index:03}")).collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .zip(&contents)
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let key = publish_tree(&source, &name, &files).await;
    let destination = repository();

    let (synced, _, _) = sync(source, &destination, &name, &key).await;
    assert!(
        synced.progress.payloads_sent >= files.len() as u64,
        "every file must arrive: {:?}",
        synced.progress.payloads_sent
    );
    let requests: u64 = synced.operations.iter().map(|(_, count)| count).sum();
    assert!(
        requests <= 8,
        "{} files cost {requests} requests: {:?}",
        files.len(),
        synced.operations
    );
    assert!(matches!(
        destination.verify_closure(&key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
}

#[tokio::test]
async fn sliced_sync_reuses_the_receivers_previous_generation() {
    let base = pseudo_random(21, 3 * 1024 * 1024 + 17);
    let new = rebuilt(&base, 128 * 1024);
    let name = RootName::try_from("system").unwrap();
    let source = repository();
    let base_key = publish_blob(&source, &name, &base).await;
    let destination = repository();

    // Nothing shared yet: the whole blob arrives as compressed literals.
    let (first, first_bytes, source) = sync(source, &destination, &name, &base_key).await;
    assert_eq!(first.progress.payloads_sent, 1);
    assert_eq!(first.progress.slice_literal_bytes, base.len() as u64);
    assert_eq!(first.progress.slice_copy_bytes, 0);
    assert!(
        first_bytes > base.len() as u64 * 9 / 10,
        "random bytes do not compress: {first_bytes}"
    );

    // The destination's current root is offered as a base for the rebuild.
    let new_key = publish_blob(&source, &name, &new).await;
    let (second, second_bytes, _) = sync(source, &destination, &name, &new_key).await;
    assert_eq!(second.progress.payloads_sent, 1);
    assert!(
        second.progress.slice_copy_bytes >= new.len() as u64 * 95 / 100,
        "{:?}",
        second.progress
    );
    assert_eq!(second.progress.slice_literal_bytes, 24 * 32);
    assert!(
        second_bytes * 100 < first_bytes,
        "wire {second_bytes} against {first_bytes}"
    );
    assert!(matches!(
        destination.verify_closure(&new_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    assert_eq!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .root(&name)
            .await
            .unwrap(),
        Some(new_key)
    );
}

async fn publish_tree(repository: &Memory, name: &RootName, files: &[(&str, &[u8])]) -> ObjectKey {
    use crate::test_util::pc;
    use crate::{Directory, Node};
    let mutation = repository.mutation_session().await.unwrap();
    let mut staged = Vec::new();
    let mut entries = Vec::new();
    for (path, bytes) in files {
        let blob = mutation.stage_blob(bytes).await.unwrap();
        entries.push((
            pc(path),
            Node::File {
                digest: blob.record().payload(),
                size: bytes.len() as u64,
                executable: false,
            },
        ));
        staged.push(blob);
    }
    let inner = Directory::try_from_iter(entries).unwrap();
    let inner_object = mutation.stage_directory(&inner).await.unwrap();
    let root = Directory::try_from_iter([(
        pc("lib"),
        Node::Directory {
            digest: inner.digest(),
            size: inner.size(),
        },
    )])
    .unwrap();
    let root_object = mutation.stage_directory(&root).await.unwrap();
    let key = root_object.record().key().clone();
    staged.push(inner_object);
    staged.push(root_object);
    mutation
        .publish(
            staged,
            vec![RootChange::Set {
                name: name.clone(),
                target: key.clone(),
            }],
        )
        .await
        .unwrap();
    key
}

#[tokio::test]
async fn sliced_sync_pairs_tree_entries_by_path() {
    let base = pseudo_random(23, 2 * 1024 * 1024);
    let new = rebuilt(&base, 128 * 1024);
    let fresh = pseudo_random(24, 256 * 1024);
    let name = RootName::try_from("system").unwrap();
    let source = repository();
    let old_key = publish_tree(
        &source,
        &name,
        &[("a.so", &base), ("same.txt", b"unchanged")],
    )
    .await;
    let destination = repository();
    let (first, _, source) = sync(source, &destination, &name, &old_key).await;
    assert_eq!(first.progress.payloads_sent, 4);

    // `a.so` is paired with the old `a.so`. `same.txt` is a complete record at
    // the destination and is skipped; `fresh.bin` and both directories ride in
    // the pipelined batch as literal-only frames.
    let new_key = publish_tree(
        &source,
        &name,
        &[
            ("a.so", &new),
            ("same.txt", b"unchanged"),
            ("fresh.bin", &fresh),
        ],
    )
    .await;
    let (second, _, _) = sync(source, &destination, &name, &new_key).await;
    let progress = &second.progress;
    assert!(
        progress.slice_copy_bytes >= new.len() as u64 * 95 / 100,
        "{progress:?}"
    );
    assert_eq!(progress.published_objects, 4, "{progress:?}");
    assert_eq!(progress.payloads_sent, 4, "{progress:?}");
    let literal = progress.slice_literal_bytes;
    let floor = 16 * 32 + fresh.len() as u64;
    assert!(literal >= floor && literal < floor + 4096, "{progress:?}");
    // One discovery answer described the whole tree and carried every
    // payload, sliced against its counterpart: no batch or payload request
    // follows it, so the sync costs one round trip after the offer.
    let operations: std::collections::BTreeMap<String, u64> =
        second.operations.iter().cloned().collect();
    assert_eq!(operations.get("discover"), Some(&1), "{operations:?}");
    assert_eq!(operations.get("objects"), None, "{operations:?}");
    assert_eq!(operations.get("payloads"), None, "{operations:?}");
    assert_eq!(operations.get("sliced"), None, "{operations:?}");
    assert!(matches!(
        destination.verify_closure(&new_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
}

#[tokio::test]
async fn sliced_sync_streams_the_whole_payload_when_a_base_is_missing() {
    let base = pseudo_random(22, 512 * 1024);
    let new = rebuilt(&base, 64 * 1024);
    let name = RootName::try_from("system").unwrap();
    let source = repository();
    let base_key = publish_blob(&source, &name, &base).await;
    let destination = repository();
    let (_, _, source) = sync(source, &destination, &name, &base_key).await;

    // The destination still names the old root but has lost its payload, so
    // the sliced frame names a source it cannot resolve.
    let base_id = crate::BlobId::new(crate::Digest::from(*blake3::hash(&base).as_bytes()));
    destination.payloads().delete_blob(&base_id).await.unwrap();
    assert!(!destination.payloads().has(&base_id).await.unwrap());

    let new_key = publish_blob(&source, &name, &new).await;
    let (result, _, _) = sync(source, &destination, &name, &new_key).await;
    assert_eq!(result.progress.payloads_sent, 1);
    assert_eq!(result.progress.slice_copy_bytes, 0);
    assert_eq!(result.progress.slice_literal_bytes, 0);
    assert!(matches!(
        destination.verify_closure(&new_key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
}

/// Publish `roots` trees that all contain one identical shared subtree plus
/// one file of their own, and return their keys in order.
async fn publish_shared(
    repository: &Memory,
    roots: usize,
    shared: &[(&str, &[u8])],
    own: &[u8],
) -> Vec<ObjectKey> {
    use crate::test_util::pc;
    use crate::{Directory, Node};
    let mutation = repository.mutation_session().await.unwrap();
    let mut staged = Vec::new();
    let mut entries = Vec::new();
    for (path, bytes) in shared {
        let blob = mutation.stage_blob(bytes).await.unwrap();
        entries.push((
            pc(path),
            Node::File {
                digest: blob.record().payload(),
                size: bytes.len() as u64,
                executable: false,
            },
        ));
        staged.push(blob);
    }
    let common = Directory::try_from_iter(entries).unwrap();
    staged.push(mutation.stage_directory(&common).await.unwrap());

    let mut keys = Vec::new();
    let mut changes = Vec::new();
    for index in 0..roots {
        // Distinct content per root: identical trees would collapse to one
        // key and the test would not exercise many roots at all.
        let mut own = own.to_vec();
        own[0] ^= index as u8;
        own[1] ^= (index >> 8) as u8;
        let mine = mutation.stage_blob(&own).await.unwrap();
        let tree = Directory::try_from_iter([
            (
                pc("shared"),
                Node::Directory {
                    digest: common.digest(),
                    size: common.size(),
                },
            ),
            (
                pc("own.bin"),
                Node::File {
                    digest: mine.record().payload(),
                    size: own.len() as u64,
                    executable: false,
                },
            ),
        ])
        .unwrap();
        let object = mutation.stage_directory(&tree).await.unwrap();
        let key = object.record().key().clone();
        staged.push(mine);
        staged.push(object);
        changes.push(RootChange::Set {
            name: RootName::try_from(format!("p/{index}")).unwrap(),
            target: key.clone(),
        });
        keys.push(key);
    }
    mutation.publish(staged, changes).await.unwrap();
    keys
}

/// Several roots sharing one subtree are paired once, not once per root.
/// Without that the pairing walk expands the shared subtree for every root
/// that reaches it, which is what exhausted memory on a real closure.
#[tokio::test]
async fn shared_subtrees_are_paired_once_across_roots() {
    const ROOTS: usize = 24;
    let shared: Vec<(&str, &[u8])> = vec![
        ("a.bin", b"shared one"),
        ("b.bin", b"shared two"),
        ("c.bin", b"shared three"),
    ];
    let base = pseudo_random(31, 256 * 1024);
    let new = rebuilt(&base, 16 * 1024);

    let source = repository();
    let destination = repository();
    // The destination holds the old generation; the source holds both, so it
    // can slice the new one against it.
    let old_keys = publish_shared(&destination, ROOTS, &shared, &base).await;
    publish_shared(&source, ROOTS, &shared, &base).await;
    let new_keys = publish_shared(&source, ROOTS, &shared, &new).await;
    assert_ne!(old_keys[0], new_keys[0]);

    let (client, server) = tokio::io::duplex(256 * 1024);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let served =
        tokio::spawn(async move { serve_transfer_stdio(&source, server_read, server_write).await });
    let session = SshTransferSession::connect(Box::new(client_read), Box::new(client_write), None)
        .await
        .unwrap();
    let request = TransferRequest {
        objects: new_keys
            .iter()
            .map(|key| ObjectRequest {
                key: key.clone(),
                recursive: true,
            })
            .collect(),
        roots: new_keys
            .iter()
            .enumerate()
            .map(|(index, key)| DestinationRoot {
                name: RootName::try_from(format!("p/{index}")).unwrap(),
                target: key.clone(),
            })
            .collect(),
    };
    let result = crate::transfer(
        &crate::sync::HeldSession(&session),
        &destination,
        request,
        crate::sync::TransferOptions::default(),
    )
    .await
    .unwrap();
    drop(session);
    served.await.unwrap().unwrap();

    let progress = result.progress;
    // Every root's own file is sliced against its counterpart, and the shared
    // subtree is already present, so almost nothing is literal.
    assert!(progress.payloads_sent >= ROOTS as u64, "{progress:?}");
    assert!(
        progress.slice_copy_bytes >= base.len() as u64 * ROOTS as u64 * 90 / 100,
        "{progress:?}"
    );
    for key in &new_keys {
        assert!(matches!(
            destination.verify_closure(key).await.unwrap(),
            ClosureStatus::Complete { .. }
        ));
    }
}
