#![cfg(feature = "experimental")]

//! Downstream-style use of the portable public API.
//!
//! This file is deliberately not feature-gated: it compiles and runs under
//! `--no-default-features` as well as the native profile, and it uses only
//! identity, encoding, format, and archive-framing types. Verification runs on
//! a hand-written executor over a payload reader implemented on a plain byte
//! slice, so nothing here can quietly start depending on Tokio, a local blob
//! store, or any other native backend without breaking the portable build.

use std::future::Future;
use std::io;
use std::task::{Context, Poll, Waker};

use casita::experimental::{
    BlobId, CasitarHeader, Digest, Directory, DirectoryId, FormatLimits, FormatRegistry, Node,
    ObjectKey, ObjectRecord, PathComponent, PayloadReader, SymlinkTarget, async_trait,
};

/// Run a future that never yields to a reactor.
///
/// Portable verification is runtime independent, so a consumer with no async
/// runtime at all can still drive it. A `Pending` here would mean a portable
/// API had grown a dependency on someone else's executor.
fn run<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("portable verification must not wait on a runtime"),
    }
}

/// A payload source with no filesystem, no runtime, and no native dependency.
struct SliceReader<'a> {
    remaining: &'a [u8],
    exact_len: u64,
}

impl<'a> SliceReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            exact_len: bytes.len() as u64,
        }
    }
}

#[async_trait]
impl PayloadReader for SliceReader<'_> {
    fn exact_len(&self) -> Option<u64> {
        Some(self.exact_len)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let take = buffer.len().min(self.remaining.len());
        buffer[..take].copy_from_slice(&self.remaining[..take]);
        self.remaining = &self.remaining[take..];
        Ok(take)
    }
}

fn name(literal: &str) -> PathComponent {
    PathComponent::try_from(literal).unwrap()
}

fn verify(key: &ObjectKey, payload: &[u8]) -> ObjectRecord {
    let registry = FormatRegistry::builtin();
    let limits = FormatLimits::default();
    let mut reader = SliceReader::new(payload);
    run(registry.verify(key, &mut reader, &limits))
        .unwrap()
        .into_record()
}

#[test]
fn a_portable_consumer_verifies_blob_and_directory_objects() {
    let contents = b"portable bytes";
    let blob = BlobId::new(Digest::hash(contents));
    let record = verify(&ObjectKey::blob(blob), contents);
    assert_eq!(record.payload(), blob);
    assert_eq!(record.payload_size(), contents.len() as u64);
    assert!(record.links().is_empty());

    let child = Directory::try_from_iter([(
        name("data"),
        Node::File {
            digest: blob,
            size: contents.len() as u64,
            executable: false,
        },
    )])
    .unwrap();
    let child_id = child.digest();

    let parent = Directory::try_from_iter([
        (
            name("nested"),
            Node::Directory {
                digest: child_id,
                size: child.size(),
            },
        ),
        (
            name("link"),
            Node::Symlink {
                target: SymlinkTarget::try_from("nested/data").unwrap(),
            },
        ),
        (
            name("run"),
            Node::File {
                digest: blob,
                size: contents.len() as u64,
                executable: true,
            },
        ),
    ])
    .unwrap();

    let encoded = parent.encode();
    let key = ObjectKey::directory(parent.digest());
    let record = verify(&key, &encoded);
    assert_eq!(record.key(), &key);
    assert_eq!(record.payload_size(), encoded.len() as u64);
    // Canonical forward links: the blob and the child directory, each once, in
    // canonical key order rather than entry order.
    assert_eq!(
        record.links(),
        &[ObjectKey::blob(blob), ObjectKey::directory(child_id)]
    );

    // The same bytes under a different identity are rejected, so a consumer
    // cannot publish a mislabelled object.
    let wrong = ObjectKey::directory(DirectoryId::new(Digest::hash(b"not this tree")));
    let registry = FormatRegistry::builtin();
    let limits = FormatLimits::default();
    let mut reader = SliceReader::new(&encoded);
    assert!(run(registry.verify(&wrong, &mut reader, &limits)).is_err());
}

#[test]
fn a_portable_consumer_round_trips_directory_and_archive_encodings() {
    let directory = Directory::try_from_iter([(
        name("file"),
        Node::File {
            digest: BlobId::new(Digest::hash(b"archived")),
            size: 8,
            executable: false,
        },
    )])
    .unwrap();
    let encoded = directory.encode();
    assert_eq!(Directory::decode(&encoded).unwrap(), directory);

    let header = CasitarHeader::new(vec![ObjectKey::directory(directory.digest())]).unwrap();
    let bytes = header.encode();
    let (decoded, consumed) = CasitarHeader::decode_prefix(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(decoded.roots(), header.roots());
}
