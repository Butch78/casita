use crate::{
    filesystem::*,
    repository::{Backend, Entry},
};
use casita_fs::FilesystemNodeKind;
use std::io;

const STATS_ID: u64 = 1 << 48;
fn stats_entry(id: u64, size: usize) -> Metadata {
    Metadata {
        id,
        parent: 2,
        kind: FileKind::File,
        size: size as u64,
        mode: 0o444,
    }
}

fn error(error: anyhow::Error) -> io::Error {
    match error.downcast::<io::Error>() {
        Ok(error) => error,
        Err(error) => io::Error::other(error),
    }
}

fn metadata(entry: &Entry) -> Metadata {
    Metadata {
        id: entry.id,
        parent: entry.parent,
        size: entry.size(),
        mode: entry.mode(),
        kind: match entry.kind() {
            FilesystemNodeKind::Directory => FileKind::Directory,
            FilesystemNodeKind::Regular => FileKind::File,
            FilesystemNodeKind::Symlink => FileKind::Symlink,
        },
    }
}

/// Request to publish a pre-staged root. Descriptor verification stays in Casita.
pub struct PublishRoot {
    pub name: Vec<u8>,
    pub kind: FileKind,
    pub target: Option<Vec<u8>>,
}

impl Filesystem for Backend {
    fn root(&self) -> u64 {
        2
    }

    fn metadata(&self, id: u64) -> io::Result<Metadata> {
        if id >= STATS_ID {
            return Ok(stats_entry(id, self.stats_file_size()));
        }
        self.entry(id).map(|entry| metadata(&entry)).map_err(error)
    }

    fn lookup(&self, parent: u64, name: &[u8]) -> io::Result<Option<Metadata>> {
        if parent == self.root() {
            if let Some(snapshot) = name
                .strip_prefix(b"__casita_stats-")
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.parse::<u32>().ok())
            {
                return Ok(Some(stats_entry(
                    STATS_ID + u64::from(snapshot),
                    self.stats_file_size(),
                )));
            }
        }
        if self.metadata(parent)?.kind != FileKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        Backend::lookup(self, parent, name)
            .map(|entry| entry.as_ref().map(metadata))
            .map_err(error)
    }

    fn directory(&self, id: u64) -> io::Result<DirectorySnapshot> {
        if self.metadata(id)?.kind != FileKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        self.directory_snapshot(id, |entries| {
            let mut entries = entries
                .into_iter()
                .map(|entry| {
                    let metadata = metadata(&entry);
                    DirectoryEntry {
                        name: entry.name,
                        id: entry.id,
                        kind: metadata.kind,
                        metadata: Some(metadata),
                    }
                })
                .collect::<Vec<_>>();
            if id == self.root() {
                let metadata = stats_entry(STATS_ID, self.stats_file_size());
                entries.push(DirectoryEntry {
                    name: b"__casita_stats-0".to_vec(),
                    id: STATS_ID,
                    kind: FileKind::File,
                    metadata: Some(metadata),
                });
            }
            DirectorySnapshot(entries.into())
        })
        .map_err(error)
    }

    fn read(&self, id: u64, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        if self.metadata(id)?.kind != FileKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        // Preserve the existing reader/cache implementation during extraction.
        // Direct reads into the supplied buffer are a separate performance change.
        let bytes = Filesystem::read_data(self, id, offset, output.len())?;
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn read_link(&self, id: u64) -> io::Result<Vec<u8>> {
        self.entry(id)
            .map_err(error)?
            .node
            .and_then(|node| node.symlink_target().map(ToOwned::to_owned))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a symlink"))
    }

    fn shutdown(&mut self) -> io::Result<()> {
        self.flush().map_err(error)?;
        let report =
            serde_json::json!({"repository_release_barrier":"passed", "stats":self.stats()});
        std::fs::write(self.path.join("native-final.json"), report.to_string())
    }

    fn read_data(
        &self,
        id: u64,
        offset: u64,
        length: usize,
    ) -> io::Result<std::borrow::Cow<'_, [u8]>> {
        if id >= STATS_ID {
            let mut bytes = self.stats().to_string().into_bytes();
            if bytes.len() > self.stats_file_size() {
                return Err(io::Error::other(
                    "statistics exceed diagnostic file capacity",
                ));
            }
            bytes.resize(self.stats_file_size(), b' ');
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            return Ok(std::borrow::Cow::Owned(
                bytes[start..].iter().copied().take(length).collect(),
            ));
        }
        Backend::read(self, id, offset, length.min(u32::MAX as usize) as u32)
            .map(std::borrow::Cow::Owned)
            .map_err(error)
    }

    fn options(&self) -> VolumeOptions {
        self.config.volume
    }

    fn create(
        &self,
        parent: u64,
        name: &[u8],
        kind: FileKind,
        target: Option<&[u8]>,
    ) -> io::Result<Metadata> {
        if parent != 3 {
            return Err(io::Error::new(
                io::ErrorKind::ReadOnlyFilesystem,
                "publication requires views directory",
            ));
        }
        self.control(PublishRoot {
            name: name.to_vec(),
            kind,
            target: target.map(ToOwned::to_owned),
        })
    }

    fn observe(&self, event: Observation<'_>) {
        match event {
            Observation::Xattr { operation, name } => {
                self.reader.counters.record_xattr(operation, name)
            }
            Observation::Read {
                id,
                backend_ns,
                copy_ns,
                reply_ns,
                error,
            } => {
                if id < STATS_ID {
                    self.reader
                        .counters
                        .record_read_callback(backend_ns, copy_ns, reply_ns, error);
                }
            }
            Observation::Enumeration {
                parent,
                attributes,
                initial,
                entries,
                nanos,
                allocations,
                phases,
            } => {
                self.reader.counters.record_enumeration(
                    parent,
                    attributes,
                    initial,
                    entries,
                    nanos,
                    allocations,
                );
                if let Some(phases) = phases {
                    self.reader
                        .counters
                        .record_enumeration_phases(parent, attributes, initial, phases);
                }
            }
        }
    }
}

impl Control for Backend {
    type Request = PublishRoot;
    type Reply = Metadata;
    fn control(&self, request: PublishRoot) -> io::Result<Metadata> {
        let kind = match request.kind {
            FileKind::Directory => FilesystemNodeKind::Directory,
            FileKind::File => FilesystemNodeKind::Regular,
            FileKind::Symlink => FilesystemNodeKind::Symlink,
        };
        self.publish_checked(&request.name, kind, request.target.as_deref())
            .map(|entry| metadata(&entry))
            .map_err(error)
    }
}
