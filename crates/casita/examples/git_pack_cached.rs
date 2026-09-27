//! Direct local cached pack generation; no HTTP server or network client.
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use casita::experimental::*;
use casita::import::GitImport;
use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

#[derive(Default)]
struct Measurements {
    read_ends: HashMap<u64, Instant>,
    reads: Vec<f64>,
    encodes: Vec<f64>,
    handoffs: Vec<f64>,
}

#[derive(Clone, Default)]
struct Timings(Arc<Mutex<Measurements>>);

#[derive(Default)]
struct Index(Option<u64>);
impl tracing::field::Visit for Index {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "index" {
            self.0 = Some(value);
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

struct Started {
    at: Instant,
    index: u64,
}

impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Timings {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        let span = ctx.span(id).unwrap();
        let mut index = Index::default();
        attrs.record(&mut index);
        let index = index
            .0
            .expect("payload timing span must carry its catalog index");
        let at = Instant::now();
        if span.name() == "git.fetch.encode_object" {
            let mut data = self.0.lock().unwrap();
            let finished = data
                .read_ends
                .remove(&index)
                .expect("encoding follows its payload read");
            data.handoffs
                .push(at.duration_since(finished).as_secs_f64());
        }
        span.extensions_mut().insert(Started { at, index });
    }

    fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).unwrap();
        let extensions = span.extensions();
        let started = extensions.get::<Started>().unwrap();
        let end = Instant::now();
        let mut data = self.0.lock().unwrap();
        if span.name() == "git.fetch.read_payload" {
            data.reads
                .push(end.duration_since(started.at).as_secs_f64());
            assert!(data.read_ends.insert(started.index, end).is_none());
        } else {
            data.encodes
                .push(end.duration_since(started.at).as_secs_f64());
        }
    }
}

fn distribution(values: &mut [f64]) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let at = |fraction: f64| {
        values
            .get(((values.len().saturating_sub(1)) as f64 * fraction) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    serde_json::json!({"count":values.len(), "sum_seconds":values.iter().sum::<f64>(),
        "p50_seconds":at(0.5), "p95_seconds":at(0.95), "max_seconds":at(1.0)})
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() >= 4,
        "prepare REPO SOURCE | run REPO PACK_FILE EXPECTED_COMMIT"
    );
    let repository = Repository::local_with_pack_options(
        &args[2],
        casita::experimental::PackOptions {
            target_size: 4 * 1024 * 1024,
            cache_capacity: 128 * 1024 * 1024,
        },
    )
    .await?;
    if args[1] == "prepare" {
        repository
            .import(
                GitImport::new(&args[3], "snapshot")
                    .with_refs(["refs/heads/main"])?
                    .with_max_cached_pack_bytes(0),
            )
            .await?;
        return Ok(());
    }
    assert_eq!(args[1], "run");
    assert_eq!(args.len(), 5);
    let (_, view) = read_git_view(&repository, "snapshot")
        .await?
        .ok_or("missing view")?;
    assert!(view.pack.is_none(), "native pack cache must be disabled");
    let GitRefValue::Direct(tip) = &view.refs[&CanonicalRefName::try_from("refs/heads/main")?]
    else {
        return Err("fixture main must be direct".into());
    };
    assert_eq!(
        tip.native_id(),
        gix_hash::ObjectId::from_hex(args[4].as_bytes())?.as_bytes()
    );
    let request = GitFetchRequest {
        wants: vec![tip.native_id().to_vec()],
        haves: vec![],
        depth: None,
        done: true,
        multi_ack_detailed: false,
        side_band_64k: false,
    };
    let mut limits = GitFetchLimits::default();
    limits.max_pack_bytes = 8 * 1024 * 1024 * 1024;
    let service = GitFetchService::bind(&repository, "snapshot", limits).await?;

    let enabled = Arc::new(AtomicBool::new(false));
    let timings = Timings::default();
    let filter_enabled = enabled.clone();
    tracing_subscriber::registry()
        .with(
            timings
                .clone()
                .with_filter(tracing_subscriber::filter::dynamic_filter_fn(
                    move |metadata, _| {
                        filter_enabled.load(Ordering::Relaxed)
                            && matches!(
                                metadata.name(),
                                "git.fetch.read_payload" | "git.fetch.encode_object"
                            )
                    },
                )),
        )
        .init();
    println!(
        "{}",
        serde_json::json!({"event":"ready", "objects":view.objects.len()})
    );
    std::io::stdout().flush()?;
    // The process waits for the driver to validate each pack before accepting
    // another command. File writes, validation and statistics are outside timing.
    for line in std::io::stdin().lock().lines() {
        let command: serde_json::Value = serde_json::from_str(&line?)?;
        let batch = command["batch"].as_u64().ok_or("missing batch")? as usize;
        let diagnostics = command["diagnostics"].as_bool().unwrap_or(false);
        let pipelined = command["pipelined"].as_bool().unwrap_or(false);
        *timings.0.lock().unwrap() = Measurements::default();
        enabled.store(diagnostics, Ordering::Relaxed);
        let before = repository.payloads().pack_read_stats().unwrap();
        let start = Instant::now();
        let pack = if pipelined {
            service
                .benchmark_build_pack_pipelined(&request, batch)
                .await?
        } else {
            service.benchmark_build_pack(&request, batch).await?
        };
        let elapsed = start.elapsed().as_secs_f64();
        enabled.store(false, Ordering::Relaxed);
        let after = repository.payloads().pack_read_stats().unwrap();
        assert!(pack.shallow.is_empty());
        assert_eq!(
            u32::from_be_bytes(pack.pack[8..12].try_into()?) as usize,
            view.objects.len()
        );
        let digest = blake3::hash(&pack.pack).to_hex().to_string();
        std::fs::write(&args[3], &pack.pack)?;
        let mut data = timings.0.lock().unwrap();
        assert!(data.read_ends.is_empty());
        println!(
            "{}",
            serde_json::json!({"event":"sample", "batch":batch,
            "pipelined":pipelined,
            "diagnostics":diagnostics, "seconds":elapsed, "pack_bytes":pack.pack.len(), "pack_blake3":digest,
            "payload_requests":after.chunk_range_requests + after.whole_pack_requests - before.chunk_range_requests - before.whole_pack_requests,
            "payload_bytes":after.chunk_range_bytes + after.whole_pack_bytes - before.chunk_range_bytes - before.whole_pack_bytes,
            "read":distribution(&mut data.reads), "encode":distribution(&mut data.encodes),
            "handoff":distribution(&mut data.handoffs)})
        );
        std::io::stdout().flush()?;
    }
    Ok(())
}
