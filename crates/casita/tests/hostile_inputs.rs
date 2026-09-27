#![cfg(feature = "experimental")]

//! Small deterministic hostile-input cases selected for Miri.
//!
//! This is deliberately not a substitute for the mutation corpus in `fuzz/`.

use casita::experimental::{
    CasitarFrameHeader, CasitarHeader, Directory, GitObjectFormat, GitViewBody, LinkedIpld,
    decode_object_key, decode_object_record, decode_root_record, parse_git_tree,
};

#[test]
fn portable_decoders_reject_truncation_and_hostile_lengths() {
    let hostile = [
        &[][..],
        &[0][..],
        &u64::MAX.to_le_bytes()[..],
        b"casitar1\xff\xff\xff\xff\xff\xff\xff\xff",
    ];
    for input in hostile {
        let _ = decode_object_key(input);
        let _ = decode_root_record(input);
        let _ = decode_object_record(input);
        let _ = Directory::decode(input);
        let _ = LinkedIpld::decode(input, 64);
        let _ = GitViewBody::decode(input);
        let _ = CasitarHeader::decode_prefix(input);
        let _ = CasitarFrameHeader::decode_prefix(input);
        let _ = parse_git_tree(GitObjectFormat::Sha1, input);
        let _ = parse_git_tree(GitObjectFormat::Sha256, input);
    }
}
