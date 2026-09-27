#![no_main]

use bytes::Bytes;
use casita::experimental::{PathComponent, SshEndpoint, SymlinkTarget};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    match selector % 3 {
        0 => {
            let _ = PathComponent::try_from(Bytes::copy_from_slice(body));
        }
        1 => {
            let _ = SymlinkTarget::try_from(Bytes::copy_from_slice(body));
        }
        2 => {
            let _ = std::str::from_utf8(body)
                .ok()
                .and_then(|endpoint| endpoint.parse::<SshEndpoint>().ok());
        }
        _ => {}
    }
});
