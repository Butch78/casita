//! Deterministic memory fixture for native FSKit transport benchmarks.
use crate::filesystem::{self, *};
use std::{collections::HashMap, io, ops::Deref};

pub const ROOT_ID: u64 = 2;
pub const FIRST_FILE_ID: u64 = 3;

/// Immutable memory backend for native FSKit transport measurements.
/// Transport adapters only map inode numbers, attributes, buffers and replies.
pub struct MemoryFilesystem {
    entries: Vec<Entry>,
    names: HashMap<Vec<u8>, u64>,
    snapshot: std::sync::OnceLock<filesystem::DirectorySnapshot>,
}

impl Default for MemoryFilesystem {
    fn default() -> Self {
        let entries = fixture();
        let names = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.name.clone(), index as u64 + FIRST_FILE_ID))
            .collect();
        Self {
            entries,
            names,
            snapshot: std::sync::OnceLock::new(),
        }
    }
}

impl Deref for MemoryFilesystem {
    type Target = [Entry];
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl MemoryFilesystem {
    pub fn lookup(&self, name: &[u8]) -> Option<u64> {
        self.names.get(name).copied()
    }
    pub fn entry(&self, id: u64) -> Option<&Entry> {
        self.entries
            .get(usize::try_from(id.checked_sub(FIRST_FILE_ID)?).ok()?)
    }
    pub fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> Option<usize> {
        let bytes = self.range(id, offset, output.len())?;
        output[..bytes.len()].copy_from_slice(bytes);
        Some(bytes.len())
    }

    pub fn range(&self, id: u64, offset: u64, length: usize) -> Option<&[u8]> {
        let entry = self.entry(id).filter(|entry| !entry.symlink)?;
        let remaining = usize::try_from(offset)
            .ok()
            .and_then(|offset| entry.data.get(offset..))
            .unwrap_or_default();
        Some(&remaining[..remaining.len().min(length)])
    }
}

pub const SIZES: &[usize] = &[
    0, 1, 4095, 4096, 4097, 16383, 16384, 16385, 65535, 65536, 65537, 131071, 131072, 131073,
    1048575, 1048576, 1048577,
];
pub const SCRIPT: &[u8] = b"#!/bin/sh\nprintf 'casita-native-fskit-ok\\n'\n";

#[derive(Debug)]
pub struct Entry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub symlink: bool,
    pub executable: bool,
}

impl Entry {
    pub fn mode(&self) -> u16 {
        if self.executable {
            0o555
        } else {
            0o444
        }
    }
}

pub fn fixture() -> Vec<Entry> {
    let mut entries = Vec::new();
    for &size in SIZES {
        entries.push(Entry {
            name: format!("size-{size}").into_bytes(),
            data: (0..size).map(|i| ((i * 31 + size) % 251) as u8).collect(),
            symlink: false,
            executable: false,
        });
    }
    for index in 0..256 {
        entries.push(Entry {
            name: format!("meta-{index:04}").into_bytes(),
            data: format!("metadata-{index}\n").into_bytes(),
            symlink: false,
            executable: false,
        });
    }
    entries.push(Entry {
        name: b"run".to_vec(),
        data: SCRIPT.to_vec(),
        symlink: false,
        executable: true,
    });
    entries.push(Entry {
        name: b"link".to_vec(),
        data: b"size-4096".to_vec(),
        symlink: true,
        executable: false,
    });
    entries.push(Entry {
        name: if cfg!(feature = "portable-names") {
            b"byte-ascii".to_vec()
        } else {
            b"byte-\xff".to_vec()
        },
        data: b"byte-safe\n".to_vec(),
        symlink: false,
        executable: false,
    });
    entries
}

fn missing() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "unknown inode")
}

impl Filesystem for MemoryFilesystem {
    fn root(&self) -> u64 {
        ROOT_ID
    }

    fn metadata(&self, id: u64) -> io::Result<Metadata> {
        if id == ROOT_ID {
            return Ok(Metadata {
                id,
                parent: id,
                kind: FileKind::Directory,
                size: 0,
                mode: 0o555,
            });
        }
        let entry = self.entry(id).ok_or_else(missing)?;
        Ok(Metadata {
            id,
            parent: ROOT_ID,
            kind: if entry.symlink {
                FileKind::Symlink
            } else {
                FileKind::File
            },
            size: entry.data.len() as u64,
            mode: entry.mode(),
        })
    }

    fn lookup(&self, parent: u64, name: &[u8]) -> io::Result<Option<Metadata>> {
        if self.metadata(parent)?.kind != FileKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        MemoryFilesystem::lookup(self, name)
            .map(|id| self.metadata(id))
            .transpose()
    }

    fn directory(&self, id: u64) -> io::Result<DirectorySnapshot> {
        if self.metadata(id)?.kind != FileKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        if let Some(snapshot) = self.snapshot.get() {
            return Ok(snapshot.clone());
        }
        let entries = self
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let metadata = self.metadata(FIRST_FILE_ID + index as u64)?;
                Ok(DirectoryEntry {
                    name: entry.name.clone(),
                    id: metadata.id,
                    kind: metadata.kind,
                    metadata: Some(metadata),
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(self
            .snapshot
            .get_or_init(|| DirectorySnapshot(entries.into()))
            .clone())
    }

    fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        if self.metadata(id)?.kind != FileKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        MemoryFilesystem::read(self, id, offset, output).ok_or_else(missing)
    }

    fn read_link(&self, id: u64) -> io::Result<Vec<u8>> {
        let entry = self.entry(id).ok_or_else(missing)?;
        if !entry.symlink {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a symlink"));
        }
        Ok(entry.data.clone())
    }

    fn read_data(
        &self,
        id: u64,
        offset: u64,
        length: usize,
    ) -> io::Result<std::borrow::Cow<'_, [u8]>> {
        self.range(id, offset, length)
            .map(std::borrow::Cow::Borrowed)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"))
    }

    fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_backend_resolves_and_reads_every_byte_name() {
        let filesystem = MemoryFilesystem::default();
        for (index, entry) in filesystem.iter().enumerate() {
            let id = filesystem.lookup(&entry.name).unwrap();
            assert_eq!(id, index as u64 + FIRST_FILE_ID);
            assert_eq!(filesystem.entry(id).unwrap().name, entry.name);
            let mut output = vec![0xaa; entry.data.len() + 1];
            if entry.symlink {
                assert_eq!(filesystem.read(id, 0, &mut output), None);
            } else {
                assert_eq!(filesystem.read(id, 0, &mut output), Some(entry.data.len()));
                assert_eq!(
                    filesystem.range(id, 0, output.len()),
                    Some(entry.data.as_slice())
                );
                assert_eq!(&output[..entry.data.len()], &entry.data);
                assert_eq!(output[entry.data.len()], 0xaa);
            }
        }
        assert!(filesystem.lookup(b"missing").is_none());
        assert!(filesystem.entry(ROOT_ID).is_none());
        assert!(filesystem.entry(u64::MAX).is_none());
        let id = filesystem.lookup(b"size-4097").unwrap();
        for offset in [0, 4096, 4097, 4098, u64::MAX] {
            let mut output = [0xaa; 17];
            let count = filesystem.read(id, offset, &mut output).unwrap();
            assert_eq!(filesystem.range(id, offset, 17), Some(&output[..count]));
        }
    }

    #[test]
    fn reads_cover_boundaries_eof_and_do_not_overwrite_tail() {
        let filesystem = MemoryFilesystem::default();
        for entry in filesystem.iter().filter(|entry| !entry.symlink) {
            let id = filesystem.lookup(&entry.name).unwrap();
            for offset in [
                0,
                entry.data.len().saturating_sub(1),
                entry.data.len(),
                entry.data.len() + 1,
            ] {
                let mut buffer = [0xaa; 17];
                let count = filesystem.read(id, offset as u64, &mut buffer).unwrap();
                assert_eq!(
                    &buffer[..count],
                    &entry.data.get(offset..).unwrap_or_default()[..count]
                );
                assert_eq!(count, entry.data.len().saturating_sub(offset).min(17));
                assert!(buffer[count..].iter().all(|&byte| byte == 0xaa));
            }
        }
        let id = filesystem.lookup(b"size-1").unwrap();
        let mut buffer = [0xaa];
        assert_eq!(filesystem.read(id, u64::MAX, &mut buffer), Some(0));
        assert_eq!(buffer, [0xaa]);
    }
}
