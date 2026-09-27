//! Effective repository settings and the benchmark marker-file adapter.
use crate::filesystem::VolumeOptions;
use anyhow::Result;
use std::{io, path::Path};

#[derive(Clone, Copy, Debug)]
pub(crate) struct BackendConfig {
    pub cache_directories: bool,
    pub cache_enumeration: bool,
    pub cache_readers: bool,
    pub reader_cache_capacity: usize,
    pub volume: VolumeOptions,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            cache_directories: true,
            cache_enumeration: true,
            cache_readers: true,
            reader_cache_capacity: super::READER_CACHE_CAPACITY,
            // Preserve the repository's measured defaults, which differ from
            // the generic transport's defaults for capabilities, names and xattrs.
            volume: VolumeOptions {
                store_timestamps: true,
                explicit_capabilities: false,
                eager_attributes: false,
                filename_from_bytes: false,
                emulate_xattrs: true,
                trace_reads: false,
                time_enumeration: false,
            },
        }
    }
}

impl BackendConfig {
    /// Load the existing benchmark controls from the repository or native
    /// session directory. Backend operations consume only the resulting values.
    pub fn from_markers(path: &Path) -> Result<Self> {
        let mut config = Self {
            cache_directories: !path.join("disable-directory-cache").exists(),
            cache_enumeration: !path.join("disable-enumeration-cache").exists(),
            cache_readers: !path.join("disable-reader-cache").exists(),
            volume: VolumeOptions {
                eager_attributes: path.join("eager-enumeration-attributes").exists(),
                time_enumeration: path.join("time-enumeration-phases").exists(),
                filename_from_bytes: path.join("filename-from-bytes").exists(),
                explicit_capabilities: path.join("explicit-volume-capabilities").exists(),
                store_timestamps: !path.join("zero-timestamps").exists(),
                emulate_xattrs: !path.join("explicit-xattrs").exists(),
                trace_reads: path.join("trace-read-ranges").exists(),
            },
            ..Self::default()
        };
        match std::fs::read_to_string(path.join("reader-cache-capacity")) {
            Ok(value) => {
                config.reader_cache_capacity = match value.trim() {
                    "16" => 16,
                    "32" => 32,
                    _ => anyhow::bail!("reader-cache-capacity must be 16 or 32"),
                };
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_markers_preserve_defaults_and_overrides() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = BackendConfig::from_markers(directory.path())?;
        assert!(config.cache_directories && config.cache_enumeration && config.cache_readers);
        assert_eq!(config.reader_cache_capacity, 32);
        assert!(config.volume.store_timestamps && config.volume.emulate_xattrs);
        assert!(!config.volume.explicit_capabilities && !config.volume.filename_from_bytes);
        assert!(!config.volume.eager_attributes && !config.volume.time_enumeration);
        assert!(!config.volume.trace_reads);

        for marker in [
            "disable-directory-cache",
            "disable-enumeration-cache",
            "disable-reader-cache",
            "eager-enumeration-attributes",
            "time-enumeration-phases",
            "filename-from-bytes",
            "explicit-volume-capabilities",
            "zero-timestamps",
            "explicit-xattrs",
            "trace-read-ranges",
        ] {
            std::fs::write(directory.path().join(marker), b"")?;
        }
        std::fs::write(directory.path().join("reader-cache-capacity"), b"16\n")?;
        let config = BackendConfig::from_markers(directory.path())?;
        assert!(!config.cache_directories && !config.cache_enumeration && !config.cache_readers);
        assert_eq!(config.reader_cache_capacity, 16);
        assert!(!config.volume.store_timestamps && !config.volume.emulate_xattrs);
        assert!(config.volume.explicit_capabilities && config.volume.filename_from_bytes);
        assert!(config.volume.eager_attributes && config.volume.time_enumeration);
        assert!(config.volume.trace_reads);
        Ok(())
    }
}
