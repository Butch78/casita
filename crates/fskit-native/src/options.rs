/// Native callback choices for an immutable, byte-named filesystem.
#[derive(Clone, Copy, Debug)]
pub struct VolumeOptions {
    pub store_timestamps: bool,
    pub explicit_capabilities: bool,
    pub eager_attributes: bool,
    pub filename_from_bytes: bool,
    pub emulate_xattrs: bool,
    pub trace_reads: bool,
    pub time_enumeration: bool,
}

impl Default for VolumeOptions {
    fn default() -> Self {
        Self {
            store_timestamps: true,
            explicit_capabilities: true,
            eager_attributes: false,
            filename_from_bytes: true,
            emulate_xattrs: false,
            trace_reads: false,
            time_enumeration: false,
        }
    }
}

/// Optional observations. Backends own their counters and reporting.
pub enum Observation<'a> {
    Xattr {
        operation: &'static str,
        name: &'a [u8],
    },
    Read {
        id: u64,
        backend_ns: u64,
        copy_ns: u64,
        reply_ns: u64,
        error: bool,
    },
    Enumeration {
        parent: u64,
        attributes: bool,
        initial: bool,
        entries: u64,
        nanos: u64,
        allocations: u64,
        phases: Option<[u64; 5]>,
    },
}
