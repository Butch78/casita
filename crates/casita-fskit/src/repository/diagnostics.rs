//! Bounded diagnostic recording and the stable benchmark JSON schema.
use super::config::BackendConfig;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

#[derive(Default)]
pub struct Diagnostics {
    pub directories: AtomicU64,
    pub opens: AtomicU64,
    pub reads: AtomicU64,
    pub bytes: AtomicU64,
    pub directory_ns: AtomicU64,
    pub open_ns: AtomicU64,
    pub reader_hits: AtomicU64,
    pub reader_misses: AtomicU64,
    pub reader_evictions: AtomicU64,
    missing_lookups: Mutex<BTreeMap<String, (u64, u64)>>,
    /// Key: inode:attributes-present:initial-cookie. Values: calls, entries, ns, item allocations.
    enumerations: Mutex<BTreeMap<String, [u64; 4]>>,
    /// Setup ns, filename construction ns, packing ns, filename drop ns, attempts.
    enumeration_phases: Mutex<BTreeMap<String, [u64; 5]>>,
    xattrs: Mutex<BTreeMap<String, u64>>,
    /// inode:offset:requested length -> calls, returned bytes, elapsed ns, errors.
    read_ranges: Mutex<BTreeMap<String, [u64; 4]>>,
    read_ranges_dropped: AtomicU64,
    pub reader_lock_ns: AtomicU64,
    pub seek_ns: AtomicU64,
    pub stream_read_ns: AtomicU64,
    /// Completed FSKit read callbacks: calls, backend ns, copy ns, reply ns, errors.
    read_callbacks: Mutex<[u64; 5]>,
}
impl Diagnostics {
    pub fn record_read_callback(&self, backend_ns: u64, copy_ns: u64, reply_ns: u64, error: bool) {
        let mut row = self.read_callbacks.lock().unwrap();
        for (value, increment) in
            row.iter_mut()
                .zip([1, backend_ns, copy_ns, reply_ns, u64::from(error)])
        {
            *value += increment;
        }
    }
    pub fn record_xattr(&self, operation: &str, name: &[u8]) {
        let mut metrics = self.xattrs.lock().unwrap();
        let key = format!(
            "{operation}:{}",
            String::from_utf8_lossy(name)
                .chars()
                .take(80)
                .collect::<String>()
        );
        if metrics.len() < 16 || metrics.contains_key(&key) {
            *metrics.entry(key).or_default() += 1;
        }
    }
    pub fn record_enumeration(
        &self,
        parent: u64,
        attributes: bool,
        initial: bool,
        entries: u64,
        nanos: u64,
        allocations: u64,
    ) {
        let mut metrics = self.enumerations.lock().unwrap();
        let key = format!("{parent}:{attributes}:{initial}");
        if metrics.len() < 16 || metrics.contains_key(&key) {
            let metric = metrics.entry(key).or_default();
            metric[0] += 1;
            metric[1] += entries;
            metric[2] += nanos;
            metric[3] += allocations;
        }
    }
    pub fn record_enumeration_phases(
        &self,
        parent: u64,
        attributes: bool,
        initial: bool,
        phases: [u64; 5],
    ) {
        let mut metrics = self.enumeration_phases.lock().unwrap();
        let key = format!("{parent}:{attributes}:{initial}");
        if metrics.len() < 16 || metrics.contains_key(&key) {
            let metric = metrics.entry(key).or_default();
            for (total, value) in metric.iter_mut().zip(phases) {
                *total += value;
            }
        }
    }
    pub(super) fn record_missing_lookup(&self, parent: u64, name: &[u8], nanos: u64) {
        let mut missing = self.missing_lookups.lock().unwrap();
        let diagnostic_name = if name.starts_with(b"._size-") || name.starts_with(b"._meta-") {
            b"._<fixture-data>".as_slice()
        } else {
            name
        };
        let key = format!(
            "{parent}:{}",
            String::from_utf8_lossy(diagnostic_name)
                .chars()
                .take(80)
                .collect::<String>()
        );
        if missing.len() < 16
            || missing.contains_key(&key)
            || name == b"._run"
            || name == b"._native-executable"
        {
            let metric = missing.entry(key).or_default();
            metric.0 += 1;
            metric.1 += nanos;
        }
    }
    pub(super) fn record_read_range(
        &self,
        id: u64,
        offset: u64,
        length: u32,
        returned: Option<usize>,
        nanos: u64,
    ) {
        let key = format!("{id}:{offset}:{length}");
        let mut ranges = self.read_ranges.lock().unwrap();
        if ranges.len() < 8192 || ranges.contains_key(&key) {
            let row = ranges.entry(key).or_default();
            row[0] += 1;
            row[1] += returned.unwrap_or(0) as u64;
            row[2] += nanos;
            row[3] += u64::from(returned.is_none());
        } else {
            self.read_ranges_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub(super) fn file_size(trace_reads: bool) -> usize {
        if trace_reads {
            2 * 1024 * 1024
        } else {
            8192
        }
    }
    pub(super) fn snapshot(
        &self,
        config: &BackendConfig,
        resident: usize,
        files: BTreeMap<String, String>,
    ) -> serde_json::Value {
        let c = self;
        json!({"pid": std::process::id(), "directories": c.directories.load(Ordering::Relaxed),
            "blob_opens": c.opens.load(Ordering::Relaxed), "reads": c.reads.load(Ordering::Relaxed), "bytes": c.bytes.load(Ordering::Relaxed),
            "directory_ns": c.directory_ns.load(Ordering::Relaxed), "open_ns": c.open_ns.load(Ordering::Relaxed),
            "native_reader_cache": {"enabled":config.cache_readers, "capacity":config.reader_cache_capacity,
                "resident":resident, "hits":c.reader_hits.load(Ordering::Relaxed),
                "misses":c.reader_misses.load(Ordering::Relaxed), "evictions":c.reader_evictions.load(Ordering::Relaxed)},
            "native_read_trace": {"enabled":config.volume.trace_reads,
                "callbacks":*c.read_callbacks.lock().unwrap(),
                "reader_lock_ns":c.reader_lock_ns.load(Ordering::Relaxed),
                "seek_ns":c.seek_ns.load(Ordering::Relaxed),
                "stream_read_ns":c.stream_read_ns.load(Ordering::Relaxed),
                "ranges":*c.read_ranges.lock().unwrap(),
                "dropped":c.read_ranges_dropped.load(Ordering::Relaxed),
                "files": files},
            "native_missing_lookups": *c.missing_lookups.lock().unwrap(),
            "native_enumerations": *c.enumerations.lock().unwrap(),
            "native_enumeration_phases": *c.enumeration_phases.lock().unwrap(),
            "native_xattrs": *c.xattrs.lock().unwrap()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_preserves_benchmark_schema_and_event_totals() {
        let diagnostics = Diagnostics::default();
        diagnostics.reads.store(2, Ordering::Relaxed);
        diagnostics.bytes.store(4, Ordering::Relaxed);
        diagnostics.record_read_range(7, 0, 4, Some(4), 10);
        diagnostics.record_read_range(7, 0, 4, None, 20);
        diagnostics.record_read_callback(10, 20, 30, false);
        diagnostics.record_read_callback(1, 2, 3, true);
        diagnostics.record_missing_lookup(7, b"._size-4096", 9);
        diagnostics.record_missing_lookup(7, b"._meta-0000", 11);
        diagnostics.record_xattr("get", b"name");
        diagnostics.record_enumeration(7, true, false, 4, 6, 8);
        diagnostics.record_enumeration_phases(7, true, false, [1, 2, 3, 4, 5]);
        let config = BackendConfig {
            volume: crate::filesystem::VolumeOptions {
                trace_reads: true,
                ..BackendConfig::default().volume
            },
            ..BackendConfig::default()
        };
        let snapshot = diagnostics.snapshot(&config, 3, [("7".into(), "file".into())].into());
        assert_eq!(
            snapshot,
            json!({
                "pid": std::process::id(), "directories": 0, "blob_opens": 0,
                "reads": 2, "bytes": 4, "directory_ns": 0, "open_ns": 0,
                "native_reader_cache": {"enabled": true, "capacity": 32, "resident": 3,
                    "hits": 0, "misses": 0, "evictions": 0},
                "native_read_trace": {"enabled": true, "callbacks": [2, 11, 22, 33, 1],
                    "reader_lock_ns": 0, "seek_ns": 0, "stream_read_ns": 0,
                    "ranges": {"7:0:4": [2, 4, 30, 1]}, "dropped": 0, "files": {"7": "file"}},
                "native_missing_lookups": {"7:._<fixture-data>": [2, 20]},
                "native_enumerations": {"7:true:false": [1, 4, 6, 8]},
                "native_enumeration_phases": {"7:true:false": [1, 2, 3, 4, 5]},
                "native_xattrs": {"get:name": 1}
            })
        );
    }

    #[test]
    fn full_read_trace_updates_existing_ranges_and_counts_dropped_ranges() {
        let diagnostics = Diagnostics::default();
        for offset in 0..8192 {
            diagnostics.record_read_range(7, offset, 4, Some(4), 10);
        }
        diagnostics.record_read_range(7, 8192, 4, Some(4), 10);
        diagnostics.record_read_range(7, 0, 4, None, 20);
        let snapshot = diagnostics.snapshot(&BackendConfig::default(), 0, BTreeMap::new());
        let trace = &snapshot["native_read_trace"];
        assert_eq!(trace["dropped"], 1);
        assert_eq!(trace["ranges"].as_object().unwrap().len(), 8192);
        assert_eq!(trace["ranges"]["7:0:4"], json!([2, 4, 30, 1]));
    }
}
