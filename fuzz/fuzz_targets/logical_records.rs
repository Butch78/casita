#![no_main]

use casita::experimental::{
    NamespaceId, RepositoryRevision, RootName, TransferProgress, TransferRequest,
    decode_object_key, decode_object_record, decode_root_record,
};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    match selector % 12 {
        0 => {
            let _ = decode_object_key(body);
        }
        1 => {
            let _ = decode_root_record(body);
        }
        2 => {
            let _ = decode_object_record(body);
        }
        3 => {
            let _ = std::str::from_utf8(body)
                .ok()
                .and_then(|text| text.parse::<NamespaceId>().ok());
        }
        4 => {
            let _ = std::str::from_utf8(body)
                .ok()
                .and_then(|text| text.parse::<RootName>().ok());
        }
        5 => {
            let _ = std::str::from_utf8(body)
                .ok()
                .and_then(|text| text.parse::<RepositoryRevision>().ok());
        }
        6 => {
            let _ = casita::fuzzing::decode_manifest(body);
        }
        7 => {
            let _ = TransferRequest::decode(body);
        }
        8 => {
            let _ = TransferProgress::decode(body);
        }
        9 => {
            let _ = casita::fuzzing::decode_records(body, 4_096, MAX_INPUT);
        }
        10 => {
            let _ = casita::fuzzing::decode_presence(body, 4_096, MAX_INPUT);
        }
        _ => {
            casita::fuzzing::fuzz_sliced_payload(body);
        }
    }
});
