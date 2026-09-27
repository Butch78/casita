#![cfg(feature = "experimental")]

//! Literal vectors for the durable generic-repository encodings.
//!
//! These pin bytes, not merely the digests computed from them. A format change
//! therefore requires an explicit namespace change rather than a golden hash
//! update that conceals which layout moved.

use bytes::Bytes;
use casita::experimental::{
    BlobId, CanonicalRefName, CasitarFrameHeader, CasitarHeader, Digest, Directory, DirectoryId,
    GitObjectFormat, GitObjectKind, GitRefValue, GitViewBody, IpldCid, Node, ObjectKey,
    PathComponent, RAW_CODEC, RootName, SymlinkTarget, decode_object_key, decode_object_record,
    decode_root_record, encode_object_key, encode_object_record, encode_root_record,
    git_object_key_for_body, new_object_record, new_root_record,
};
use std::collections::{BTreeMap, BTreeSet};

fn name(bytes: &'static [u8]) -> PathComponent {
    PathComponent::try_from(Bytes::from_static(bytes)).unwrap()
}

#[test]
fn blob_payload_identity_vectors_are_frozen() {
    assert_eq!(
        Digest::hash(b"").as_bytes(),
        &[
            0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc,
            0xc9, 0x49, 0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca,
            0xe4, 0x1f, 0x32, 0x62,
        ]
    );
    assert_eq!(
        Digest::hash(b"hello").as_bytes(),
        &[
            0xea, 0x8f, 0x16, 0x3d, 0xb3, 0x86, 0x82, 0x92, 0x5e, 0x44, 0x91, 0xc5, 0xe5, 0x8d,
            0x4b, 0xb3, 0x50, 0x6e, 0xf8, 0xc1, 0x4e, 0xb7, 0x8a, 0x86, 0xe9, 0x08, 0xc5, 0x62,
            0x4a, 0x67, 0x20, 0x0f,
        ]
    );
}

#[test]
fn empty_directory_vector_is_eight_zero_bytes() {
    assert_eq!(Directory::new().encode(), [0; 8]);
}

#[test]
fn directory_with_every_entry_kind_has_literal_layout() {
    let directory = Directory::try_from_iter([
        (
            name(b"bin"),
            Node::File {
                digest: BlobId::new(Digest::from([0x22; 32])),
                size: 3,
                executable: true,
            },
        ),
        (
            name(b"empty"),
            Node::Directory {
                digest: DirectoryId::new(Digest::from([0x11; 32])),
                size: 0,
            },
        ),
        (
            name(b"link"),
            Node::Symlink {
                target: SymlinkTarget::try_from("../bin").unwrap(),
            },
        ),
        (
            name(b"readme"),
            Node::File {
                digest: BlobId::new(Digest::from([0x33; 32])),
                size: 5,
                executable: false,
            },
        ),
    ])
    .unwrap();

    let expected = [
        &b"\x04\0\0\0\0\0\0\0"[..],
        b"\x01\x03\0\0\0\0\0\0\0bin",
        &[0x22; 32],
        b"\x03\0\0\0\0\0\0\0\x01",
        b"\x00\x05\0\0\0\0\0\0\0empty",
        &[0x11; 32],
        b"\0\0\0\0\0\0\0\0",
        b"\x02\x04\0\0\0\0\0\0\0link\x06\0\0\0\0\0\0\0../bin",
        b"\x01\x06\0\0\0\0\0\0\0readme",
        &[0x33; 32],
        b"\x05\0\0\0\0\0\0\0\x00",
    ]
    .concat();
    assert_eq!(directory.encode(), expected);
    assert_eq!(Directory::decode(&expected).unwrap(), directory);
}

#[test]
fn raw_non_utf8_directory_name_is_frozen() {
    let directory = Directory::try_from_iter([(
        name(b"\xff"),
        Node::Symlink {
            target: SymlinkTarget::try_from("x").unwrap(),
        },
    )])
    .unwrap();
    let expected = b"\x01\0\0\0\0\0\0\0\x02\x01\0\0\0\0\0\0\0\xff\x01\0\0\0\0\0\0\0x";
    assert_eq!(directory.encode(), expected);
    assert_eq!(Directory::decode(expected).unwrap(), directory);
}

#[test]
fn root_record_with_unicode_and_punctuation_has_literal_layout() {
    let record = new_root_record(
        RootName::try_from("perfiles/niño!").unwrap(),
        ObjectKey::directory(DirectoryId::new(Digest::from([0x3c; 32]))),
    );
    let expected = [
        &b"\x0f\0\0\0\0\0\0\0perfiles/ni\xc3\xb1o!"[..],
        b"\x13\0\0\0\0\0\0\0casita.directory.v1",
        b"\x20\0\0\0\0\0\0\0",
        &[0x3c; 32],
    ]
    .concat();
    assert_eq!(encode_root_record(&record), expected);
    assert_eq!(decode_root_record(&expected).unwrap(), record);
    // The root vector contains an independently encoded key after its name.
    let key_bytes = &expected[8 + "perfiles/niño!".len()..];
    assert_eq!(encode_object_key(record.target()), key_bytes);
    assert_eq!(decode_object_key(key_bytes).unwrap(), *record.target());
}

#[test]
fn v0_2_object_record_with_cross_namespace_links_has_literal_layout() {
    let key = ObjectKey::blob(BlobId::new(Digest::from([0x01; 32])));
    let directory = ObjectKey::directory(DirectoryId::new(Digest::from([0x03; 32])));
    let raw_cid = IpldCid::new(RAW_CODEC, b"").unwrap();
    let raw = raw_cid.object_key().unwrap();
    let record = new_object_record(
        key,
        BlobId::new(Digest::from([0x02; 32])),
        5,
        vec![directory, raw],
    )
    .unwrap();

    let expected = [
        &b"\x0e\0\0\0\0\0\0\0casita.blob.v1"[..],
        b"\x20\0\0\0\0\0\0\0",
        &[0x01; 32],
        &[0x02; 32],
        b"\x05\0\0\0\0\0\0\0",
        b"\x02\0\0\0\0\0\0\0",
        b"\x13\0\0\0\0\0\0\0casita.directory.v1",
        b"\x20\0\0\0\0\0\0\0",
        &[0x03; 32],
        b"\x0b\0\0\0\0\0\0\0ipld.raw.v1",
        b"\x24\0\0\0\0\0\0\0",
        b"\x01\x55\x1e\x20",
        &[
            0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc,
            0xc9, 0x49, 0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca,
            0xe4, 0x1f, 0x32, 0x62,
        ],
    ]
    .concat();
    assert_eq!(encode_object_record(&record), expected);
    assert_eq!(decode_object_record(&expected).unwrap(), record);
}

#[test]
fn casitar_v1_literal_vectors() {
    let empty = BlobId::new(Digest::hash(b""));
    let root = ObjectKey::blob(empty);
    let record = new_object_record(root.clone(), empty, 0, Vec::new()).unwrap();

    let mut archive = CasitarHeader::new(vec![root]).unwrap().encode();
    archive.extend_from_slice(
        &CasitarFrameHeader::Payload {
            payload: empty,
            size: 0,
        }
        .encode()
        .unwrap(),
    );
    archive.extend_from_slice(&CasitarFrameHeader::Record(record).encode().unwrap());
    archive.extend_from_slice(&CasitarFrameHeader::End.encode().unwrap());

    let multi_root = CasitarHeader::new(vec![
        ObjectKey::blob(BlobId::new(Digest::from([0x02; 32]))),
        ObjectKey::blob(BlobId::new(Digest::from([0x01; 32]))),
    ])
    .unwrap()
    .encode();

    assert_eq!(
        data_encoding::HEXLOWER.encode(&archive),
        concat!(
            "63617369746172314e0000000000000001000000000000003e00000000000000",
            "0e000000000000006361736974612e626c6f622e76312000000000000000af13",
            "49b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f326201af",
            "1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f326200",
            "00000000000000026e000000000000000e000000000000006361736974612e62",
            "6c6f622e76312000000000000000af1349b9f5f9a1a6a0404dea36dcc9499bcb",
            "25c9adc112b7cc9a93cae41f3262af1349b9f5f9a1a6a0404dea36dcc9499bcb",
            "25c9adc112b7cc9a93cae41f32620000000000000000000000000000000000",
        )
    );
    assert_eq!(
        data_encoding::HEXLOWER.encode(&multi_root),
        concat!(
            "6361736974617231940000000000000002000000000000003e00000000000000",
            "0e000000000000006361736974612e626c6f622e763120000000000000000101",
            "0101010101010101010101010101010101010101010101010101010101013e00",
            "0000000000000e000000000000006361736974612e626c6f622e763120000000",
            "0000000002020202020202020202020202020202020202020202020202020202",
            "02020202",
        )
    );
}

#[test]
fn v0_4_literal_vectors() {
    let sha1_tree =
        git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Tree, b"").unwrap();
    let sha1_commit_body = format!(
        "tree {}\n\n",
        data_encoding::HEXLOWER.encode(sha1_tree.native_id())
    );
    let sha1_commit = git_object_key_for_body(
        GitObjectFormat::Sha1,
        GitObjectKind::Commit,
        sha1_commit_body.as_bytes(),
    )
    .unwrap();
    let sha1_tag_body = format!(
        "object {}\ntype commit\ntag v1\n\nx\n",
        data_encoding::HEXLOWER.encode(sha1_commit.native_id())
    );
    for (name, kind, body, expected) in [
        (
            "sha1-blob",
            GitObjectKind::Blob,
            b"".as_slice(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        ),
        (
            "sha1-tree",
            GitObjectKind::Tree,
            b"".as_slice(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        ),
        (
            "sha1-commit",
            GitObjectKind::Commit,
            sha1_commit_body.as_bytes(),
            "8d7ff291d28b7f1109200d31f87a6f98fe7df90e",
        ),
        (
            "sha1-tag",
            GitObjectKind::Tag,
            sha1_tag_body.as_bytes(),
            "f320cb51bafa2529d0630573cf5bbb23e27434e4",
        ),
    ] {
        let key = git_object_key_for_body(GitObjectFormat::Sha1, kind, body).unwrap();
        assert_eq!(
            data_encoding::HEXLOWER.encode(key.native_id()),
            expected,
            "{name}"
        );
    }
    let sha256_tree =
        git_object_key_for_body(GitObjectFormat::Sha256, GitObjectKind::Tree, b"").unwrap();
    let sha256_commit_body = format!(
        "tree {}\n\n",
        data_encoding::HEXLOWER.encode(sha256_tree.native_id())
    );
    let sha256_commit = git_object_key_for_body(
        GitObjectFormat::Sha256,
        GitObjectKind::Commit,
        sha256_commit_body.as_bytes(),
    )
    .unwrap();
    let sha256_tag_body = format!(
        "object {}\ntype commit\ntag v1\n\nx\n",
        data_encoding::HEXLOWER.encode(sha256_commit.native_id())
    );
    for (name, kind, body, expected) in [
        (
            "sha256-blob",
            GitObjectKind::Blob,
            b"".as_slice(),
            "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813",
        ),
        (
            "sha256-tree",
            GitObjectKind::Tree,
            b"".as_slice(),
            "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321",
        ),
        (
            "sha256-commit",
            GitObjectKind::Commit,
            sha256_commit_body.as_bytes(),
            "e3dabff994876d150e3ec81d8bb68551cfa65896f58dcab85ed213cc0cf90293",
        ),
        (
            "sha256-tag",
            GitObjectKind::Tag,
            sha256_tag_body.as_bytes(),
            "471e9072293db1a2342f4afc3242892ce7e67dabbccf8c8b121fafe6db3a4ebb",
        ),
    ] {
        let key = git_object_key_for_body(GitObjectFormat::Sha256, kind, body).unwrap();
        assert_eq!(
            data_encoding::HEXLOWER.encode(key.native_id()),
            expected,
            "{name}"
        );
    }
    let main = CanonicalRefName::try_from("refs/heads/main").unwrap();
    let view = GitViewBody {
        object_format: GitObjectFormat::Sha1,
        refs: BTreeMap::from([(main.clone(), GitRefValue::Direct(sha1_commit.clone()))]),
        default_ref: Some(main),
        pack: None,
        objects: BTreeSet::from([sha1_commit]),
    };
    assert_eq!(
        data_encoding::HEXLOWER.encode(&view.encode().unwrap()),
        "6361736974612d6769742d766965772d7631000101000000000000000f00000000000000726566732f68656164732f6d61696e00360000000000000012000000000000006769742e736861312e636f6d6d69742e763114000000000000008d7ff291d28b7f1109200d31f87a6f98fe7df90e0100000000000000360000000000000012000000000000006769742e736861312e636f6d6d69742e763114000000000000008d7ff291d28b7f1109200d31f87a6f98fe7df90e010f00000000000000726566732f68656164732f6d61696e00"
    );
}
