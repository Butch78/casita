//! Optional wall-clock diagnostics, including waits and failed phases.
use std::time::Instant;

pub(crate) struct CollectionPhase {
    name: &'static str,
    started: Option<Instant>,
}

impl CollectionPhase {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: tracing::enabled!(target: "casita::collection_timing", tracing::Level::DEBUG)
                .then(Instant::now),
        }
    }
}

impl Drop for CollectionPhase {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            tracing::debug!(
                target: "casita::collection_timing",
                phase = self.name,
                elapsed_seconds = started.elapsed().as_secs_f64(),
            );
        }
    }
}
