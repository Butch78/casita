//! Deterministic unique payloads shared by NAR regression and performance cases.
pub fn archive(files: usize, size: usize) -> Vec<u8> {
    build(files, size, false)
}

pub fn nested_archive(files: usize, size: usize) -> Vec<u8> {
    build(files, size, true)
}

fn build(files: usize, size: usize, nested: bool) -> Vec<u8> {
    use nix_archive::nar::{NamedNode, Node};
    let per_directory = files;
    let files = if nested { files * 3 } else { files };
    let names: Vec<_> = (0..files).map(|i| format!("file-{i:08}")).collect();
    let payloads: Vec<Vec<u8>> = (0..files)
        .map(|i| {
            let hash = blake3::hash(&(i as u64).to_le_bytes());
            (0..size)
                .map(|offset| hash.as_bytes()[offset % 32])
                .collect()
        })
        .collect();
    let children: Vec<_> = names
        .iter()
        .zip(&payloads)
        .enumerate()
        .map(|(i, (name, payload))| NamedNode {
            name: name.as_bytes(),
            node: Node::Regular {
                executable: i % 2 == 0,
                contents: payload,
            },
        })
        .collect();
    let mut bytes = Vec::new();
    let names = [b"a", b"b", b"c"];
    let directories: Vec<_> = if nested {
        children
            .chunks(per_directory)
            .zip(names)
            .map(|(entries, name)| NamedNode {
                name,
                node: Node::Directory(entries),
            })
            .collect()
    } else {
        Vec::new()
    };
    let entries = if nested { &directories } else { &children };
    nix_archive::nar::encode_tree(&mut bytes, &Node::Directory(entries)).unwrap();
    bytes
}

/// Unique directory payloads without file writes obscuring their admissions.
pub fn directory_archive(directories: usize, links: usize, target_size: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = nix_archive::nar::Encoder::new(&mut bytes).unwrap();
    encoder.start_directory(None).unwrap();
    for index in 0..directories {
        let name = format!("dir-{index:08}");
        encoder.start_directory(Some(name.as_bytes())).unwrap();
        for link in 0..links {
            let name = format!("link-{link:08}");
            let mut target = format!("{index:08}-{link:08}").into_bytes();
            target.resize(target_size, b'x');
            encoder.symlink(Some(name.as_bytes()), &target).unwrap();
        }
        encoder.end_directory().unwrap();
    }
    encoder.end_directory().unwrap();
    encoder.finish().unwrap();
    bytes
}

pub fn nested_directories(depth: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = nix_archive::nar::Encoder::new(&mut bytes).unwrap();
    encoder.start_directory(None).unwrap();
    for _ in 0..depth {
        encoder.start_directory(Some(b"child")).unwrap();
    }
    encoder.symlink(Some(b"link"), b"target").unwrap();
    for _ in 0..=depth {
        encoder.end_directory().unwrap();
    }
    encoder.finish().unwrap();
    bytes
}

/// Distinct roots exercise repeated mutations rather than association hits.
pub fn sequence(imports: usize) -> Vec<Vec<u8>> {
    (0..imports)
        .map(|index| {
            let mut payload = vec![b'x'; 1024];
            payload[..8].copy_from_slice(&(index as u64).to_le_bytes());
            let mut bytes = Vec::new();
            nix_archive::nar::encode_regular(&mut bytes, &payload, false).unwrap();
            bytes
        })
        .collect()
}
