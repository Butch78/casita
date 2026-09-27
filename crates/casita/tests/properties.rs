//! Randomized property tests over casita's public invariants.

use bytes::Bytes;
use casita::{
    BlobId, Digest, Directory, DirectoryDecodeError, DirectoryId, Node, PathComponent,
    SymlinkTarget,
};
use proptest::prelude::*;

fn arb_digest() -> impl Strategy<Value = Digest> {
    any::<[u8; 32]>().prop_map(Digest::from)
}

fn arb_node() -> impl Strategy<Value = Node> {
    prop_oneof![
        (arb_digest(), 0u64..1_000_000, any::<bool>()).prop_map(|(digest, size, executable)| {
            Node::File {
                digest: BlobId::new(digest),
                size,
                executable,
            }
        }),
        (arb_digest(), 0u64..1_000_000).prop_map(|(digest, size)| Node::Directory {
            digest: DirectoryId::new(digest),
            size,
        }),
        "[a-z/.]{1,20}".prop_map(|target| Node::Symlink {
            target: SymlinkTarget::try_from(target.as_str()).unwrap(),
        }),
    ]
}

fn arb_entries() -> impl Strategy<Value = Vec<(PathComponent, Node)>> {
    // hash_map keys are unique, so `try_from_iter` never hits a duplicate name.
    prop::collection::hash_map("[a-z][a-z0-9_.]{0,10}", arb_node(), 0..8).prop_map(|m| {
        m.into_iter()
            .map(|(name, node)| (PathComponent::try_from(name.as_str()).unwrap(), node))
            .collect()
    })
}

proptest! {
    #[test]
    fn directory_encoding_roundtrip(entries in arb_entries()) {
        let directory = Directory::try_from_iter(entries).unwrap();
        let decoded: Result<Directory, DirectoryDecodeError> = Directory::decode(&directory.encode());
        prop_assert_eq!(decoded.unwrap(), directory);
    }

    /// A directory's digest depends only on its contents, not on the order the
    /// entries were inserted.
    #[test]
    fn directory_digest_is_order_independent(entries in arb_entries()) {
        let forward = Directory::try_from_iter(entries.clone()).unwrap();
        let mut reversed = entries;
        reversed.reverse();
        let backward = Directory::try_from_iter(reversed).unwrap();

        prop_assert_eq!(forward.digest(), backward.digest());
        prop_assert_eq!(forward, backward);
    }

    /// `Display` and `FromStr` round-trip for any digest.
    #[test]
    fn digest_string_roundtrip(bytes in any::<[u8; 32]>()) {
        let digest = Digest::from(bytes);
        let parsed: Digest = digest.to_string().parse().unwrap();
        prop_assert_eq!(digest, parsed);
    }

    /// A validated symlink target preserves its bytes exactly, over the full
    /// range it accepts: any non-NUL bytes, including spaces, control bytes, and
    /// bytes >= 0x80 that are not valid UTF-8.
    #[test]
    fn symlink_target_preserves_bytes(target in prop::collection::vec(1u8..=255, 1..50)) {
        let parsed = SymlinkTarget::try_from(Bytes::from(target.clone())).unwrap();
        prop_assert_eq!(parsed.as_bytes(), target.as_slice());
    }
}
