//! Optional wall-clock diagnostics, including waits and failed phases.
use std::time::Instant;

pub(super) struct LedgerPhase {
    name: &'static str,
    started: Option<Instant>,
}

impl LedgerPhase {
    pub(super) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: tracing::enabled!(target: "casita::pin_timing", tracing::Level::DEBUG)
                .then(Instant::now),
        }
    }
}

impl Drop for LedgerPhase {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            tracing::debug!(
                target: "casita::pin_timing",
                phase = self.name,
                elapsed_seconds = started.elapsed().as_secs_f64(),
            );
        }
    }
}
