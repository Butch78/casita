#![no_main]

use casita::experimental::{Directory, GitViewBody, LinkedIpld};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 256 * 1024;
const MAX_LINKS: usize = 4_096;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    match selector % 3 {
        0 => {
            let _ = Directory::decode(body);
        }
        1 => {
            let _ = LinkedIpld::decode(body, MAX_LINKS);
        }
        _ => {
            let _ = GitViewBody::decode(body);
        }
    }
});
