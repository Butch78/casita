//! Phase and ledger timing for benchmarks with one active GC interval.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone, Default)]
pub struct Timings {
    phases: Arc<Mutex<Vec<(Instant, Value)>>>,
    ledger: Arc<Mutex<BTreeMap<String, Vec<f64>>>>,
}

#[derive(Default)]
struct Fields {
    phase: String,
    seconds: f64,
}
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "phase" {
            self.phase = value.into();
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_seconds" {
            self.seconds = value;
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}
impl<S: tracing::Subscriber> Layer<S> for Timings {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if event.metadata().target() == "casita::pin_timing" {
            self.ledger
                .lock()
                .unwrap()
                .entry(fields.phase)
                .or_default()
                .push(fields.seconds);
            return;
        }
        self.phases.lock().unwrap().push((
            Instant::now(),
            json!({"phase": fields.phase, "seconds": fields.seconds}),
        ));
    }
}
impl Timings {
    pub fn install() -> Self {
        let timings = Self::default();
        tracing_subscriber::registry()
            .with(
                timings
                    .clone()
                    .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                        matches!(
                            meta.target(),
                            "casita::collection_timing" | "casita::pin_timing"
                        )
                    })),
            )
            .init();
        timings
    }

    pub fn reset(&self) {
        self.clear();
        self.ledger.lock().unwrap().clear();
    }

    // All operations through the joined workload and release barrier, excluding
    // setup and final integrity/cleanup; concurrent durations can overlap.
    pub fn take_ledger(&self) -> Value {
        let values: BTreeMap<_, _> = std::mem::take(&mut *self.ledger.lock().unwrap())
            .into_iter()
            .map(|(phase, mut seconds)| {
                seconds.sort_by(f64::total_cmp);
                let count = seconds.len();
                let sum: f64 = seconds.iter().sum();
                let p99 = seconds[(count * 99).div_ceil(100) - 1];
                (
                    phase,
                    json!({"count": count, "seconds": sum, "p99_ms": p99 * 1000.0}),
                )
            })
            .collect();
        json!(values)
    }

    pub fn clear(&self) {
        self.phases.lock().unwrap().clear();
    }

    pub fn take(&self, start: Instant) -> Vec<Value> {
        std::mem::take(&mut *self.phases.lock().unwrap())
            .into_iter()
            .map(|(finished, mut value)| {
                value["finished_seconds"] = json!(finished.duration_since(start).as_secs_f64());
                value
            })
            .collect()
    }
}
