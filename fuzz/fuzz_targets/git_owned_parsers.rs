#![no_main]

use casita::experimental::{
    CanonicalRefName, FormatLimits, GitNativeObjectFormat, GitObjectFormat, GitObjectKind,
    GitViewBody, ObjectFormat, PayloadReader, VerificationContext, git_object_key_for_body,
    parse_git_tree,
};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT: usize = 256 * 1024;

struct ExactReader<'a> {
    body: &'a [u8],
    position: usize,
}

#[casita::experimental::async_trait]
impl PayloadReader for ExactReader<'_> {
    fn exact_len(&self) -> Option<u64> {
        Some(self.body.len() as u64)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = &self.body[self.position..];
        let read = remaining.len().min(buffer.len());
        buffer[..read].copy_from_slice(&remaining[..read]);
        self.position += read;
        Ok(read)
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let format = if selector & 1 == 0 {
        GitObjectFormat::Sha1
    } else {
        GitObjectFormat::Sha256
    };
    let kind = match (selector >> 1) % 3 {
        0 => GitObjectKind::Tree,
        1 => GitObjectKind::Commit,
        _ => GitObjectKind::Tag,
    };

    let _ = parse_git_tree(format, body);
    let _ = GitViewBody::decode(body);
    let _ = std::str::from_utf8(body)
        .ok()
        .and_then(|name| CanonicalRefName::try_from(name).ok());

    let key = git_object_key_for_body(format, kind, body).expect("bounded body has a Git ID");
    let verifier = GitNativeObjectFormat::new(format, kind);
    let limits = FormatLimits {
        max_payload_bytes: MAX_INPUT as u64,
        max_metadata_bytes: MAX_INPUT as u64,
        max_links_per_object: 4_096,
        read_buffer_bytes: 4 * 1024,
        ..FormatLimits::default()
    };
    let mut reader = ExactReader { body, position: 0 };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread Tokio runtime");
    let _ = runtime.block_on(verifier.verify(VerificationContext::new(&key, &mut reader), &limits));
});
