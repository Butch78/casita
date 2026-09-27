//! Per-operation accounting for a mount.
//!
//! A store mount answers one kernel request at a time on a server thread, so
//! the interesting question when a build is slow is never "how much CPU" but
//! "how many round trips, and what did each one wait on". These counters
//! answer both: a call count and the total time inside the handler, per
//! operation, plus the bytes actually served.
//!
//! Recording is two relaxed atomic adds on a path that already crossed a
//! kernel boundary, so it stays on in production rather than hiding behind a
//! feature flag.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The operations worth separating: each has a different cost story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Resolve one path component. The tax a directory walk pays.
    Lookup,
    /// Attributes for an inode already known.
    Getattr,
    /// Open a blob, which is where a store fetch would happen.
    Open,
    /// Serve a byte range.
    Read,
    /// List a directory.
    Readdir,
    /// Resolve a symlink target.
    Readlink,
}

impl Op {
    const ALL: [Op; 6] = [
        Op::Lookup,
        Op::Getattr,
        Op::Open,
        Op::Read,
        Op::Readdir,
        Op::Readlink,
    ];

    fn index(self) -> usize {
        match self {
            Op::Lookup => 0,
            Op::Getattr => 1,
            Op::Open => 2,
            Op::Read => 3,
            Op::Readdir => 4,
            Op::Readlink => 5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Op::Lookup => "lookup",
            Op::Getattr => "getattr",
            Op::Open => "open",
            Op::Read => "read",
            Op::Readdir => "readdir",
            Op::Readlink => "readlink",
        }
    }
}

#[derive(Default)]
struct Slot {
    calls: AtomicU64,
    nanos: AtomicU64,
}

/// Live counters for one mount.
#[derive(Default)]
pub struct MountStats {
    slots: [Slot; 6],
    bytes_read: AtomicU64,
}

impl MountStats {
    /// Start timing one operation. The returned guard records on drop, so an
    /// early return through `?` is counted like any other.
    pub fn start(&self, op: Op) -> Timed<'_> {
        Timed {
            slot: &self.slots[op.index()],
            start: Instant::now(),
            _span: match op {
                Op::Lookup => tracing::trace_span!("fuse_lookup"),
                Op::Getattr => tracing::trace_span!("fuse_getattr"),
                Op::Open => tracing::trace_span!("fuse_open"),
                Op::Read => tracing::trace_span!("fuse_read"),
                Op::Readdir => tracing::trace_span!("fuse_readdir"),
                Op::Readlink => tracing::trace_span!("fuse_readlink"),
            },
        }
    }

    pub fn add_bytes_read(&self, bytes: u64) {
        self.bytes_read.fetch_add(bytes, Ordering::Relaxed);
    }

    /// A consistent-enough snapshot: counters are read one at a time, so a
    /// concurrent request can land between reads. Fine for reporting, not for
    /// asserting invariants across operations.
    #[must_use]
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            ops: Op::ALL.map(|op| {
                let slot = &self.slots[op.index()];
                (
                    op,
                    slot.calls.load(Ordering::Relaxed),
                    slot.nanos.load(Ordering::Relaxed),
                )
            }),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
        }
    }
}

/// Records elapsed time into its slot when dropped.
pub struct Timed<'a> {
    slot: &'a Slot,
    start: Instant,
    // A span lifetime measures handler service time without entering a span
    // across store futures. Disabled unless the subscriber enables trace.
    _span: tracing::Span,
}

impl Drop for Timed<'_> {
    fn drop(&mut self) {
        self.slot.calls.fetch_add(1, Ordering::Relaxed);
        self.slot
            .nanos
            .fetch_add(self.start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// What a mount served, for logging at unmount.
pub struct StatsSnapshot {
    ops: [(Op, u64, u64); 6],
    bytes_read: u64,
}

impl StatsSnapshot {
    #[must_use]
    pub fn calls(&self, op: Op) -> u64 {
        self.ops[op.index()].1
    }

    #[must_use]
    pub fn total_calls(&self) -> u64 {
        self.ops.iter().map(|(_, calls, _)| calls).sum()
    }

    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl fmt::Display for StatsSnapshot {
    /// One line per mount: `lookup=12345/3.2s mean=259us ...`, which is enough
    /// to see whether a slow build is paying for round trips or for bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (op, calls, nanos) in &self.ops {
            if *calls == 0 {
                continue;
            }
            write!(
                f,
                "{}={} in {:.3}s (mean {:.1}us) ",
                op.label(),
                calls,
                *nanos as f64 / 1e9,
                *nanos as f64 / *calls as f64 / 1e3,
            )?;
        }
        write!(f, "bytes_read={}", self.bytes_read)
    }
}
