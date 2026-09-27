//! Copied into Obrador's example directory by the permanent external-workload runner.
#[path = "../benches/support/closure.rs"]
mod closure;

use anyhow::{Result, ensure};
use obrador_core::{CasitaStore, ObradorStore};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

struct PerfControl {
    control: std::fs::File,
    ack: BufReader<std::fs::File>,
}

impl PerfControl {
    fn open() -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("CASITA_BENCH_PERF_CONTROL") else {
            return Ok(None);
        };
        let control = std::fs::OpenOptions::new().write(true).open(path)?;
        let ack = BufReader::new(std::fs::File::open(std::env::var(
            "CASITA_BENCH_PERF_ACK",
        )?)?);
        Ok(Some(Self { control, ack }))
    }

    fn command(&mut self, command: &str) -> Result<()> {
        writeln!(self.control, "{command}")?;
        let mut response = String::new();
        self.ack.read_line(&mut response)?;
        // perf versions that write sizeof("ack\\n") also send a trailing NUL,
        // which remains before the next line in the buffered FIFO reader.
        ensure!(
            response.trim_matches('\0') == "ack\n",
            "perf did not acknowledge {command}: {response:?}"
        );
        Ok(())
    }
}

async fn run(paths_count: usize, workers: usize, iterations: usize, gc: bool) -> Result<()> {
    let mut profile = PerfControl::open()?;
    let directory = tempfile::tempdir()?;
    let store = ObradorStore::open_local(directory.path(), "benchmark://retained-reads").await?;
    let nar = closure::nar()?;
    let mut paths = Vec::new();
    for _ in 0..paths_count {
        closure::add_next(&store, &nar, &mut paths).await?;
    }
    let collector = ObradorStore::open_local(directory.path(), "benchmark://retained-gc").await?;
    let verify = |store: &ObradorStore| -> Result<()> {
        ensure!(
            store.valid_paths().into_iter().collect::<BTreeSet<_>>()
                == paths.iter().cloned().collect(),
            "registered paths changed"
        );
        for (index, path) in paths.iter().enumerate() {
            let info = store.path_info(path).expect("registered metadata");
            ensure!(
                info.references.iter().cloned().collect::<BTreeSet<_>>()
                    == paths[..index].iter().rev().take(2).cloned().collect(),
                "reference graph changed"
            );
        }
        Ok(())
    };
    verify(&store)?;
    verify(&collector)?;
    // Warm the existing application path and settle setup writes before timing.
    ensure!(store.read_file(&paths[0], Path::new("")).await? == closure::PAYLOAD);
    CasitaStore::flush_releases().await?;
    let done = AtomicBool::new(false);
    if let Some(profile) = &mut profile {
        profile.command("enable")?;
    }
    let start = Instant::now();
    let reads = async {
        let result = futures::future::try_join_all((0..workers).map(|worker| {
            let store = &store;
            let paths = &paths;
            async move {
                let mut samples = Vec::new();
                for index in 0..iterations {
                    let path = &paths[(index * workers + worker) % paths.len()];
                    let start = Instant::now();
                    let bytes = store.read_file(path, Path::new("")).await?;
                    samples.push(start.elapsed().as_nanos() as u64);
                    ensure!(
                        bytes == closure::PAYLOAD,
                        "payload changed during collection"
                    );
                }
                Ok::<_, anyhow::Error>(samples)
            }
        }))
        .await;
        done.store(true, Ordering::Release);
        result
    };
    let collection = async {
        let mut samples = Vec::new();
        while gc && !done.load(Ordering::Acquire) {
            let start = Instant::now();
            collector.collect_unrooted_content().await?;
            samples.push(start.elapsed().as_nanos() as u64);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok::<_, anyhow::Error>(samples)
    };
    let (reads, collections) = tokio::join!(reads, collection);
    let elapsed = start.elapsed().as_secs_f64();
    if let Some(profile) = &mut profile {
        profile.command("disable")?;
    }
    let samples: Vec<u64> = reads?.into_iter().flatten().collect();
    let collections = collections?;
    ensure!(samples.len() == workers * iterations);
    ensure!(
        !gc || !collections.is_empty(),
        "no overlapping collection completed"
    );
    CasitaStore::flush_releases().await?;
    verify(&store)?;
    verify(&collector)?;
    for path in &paths {
        ensure!(collector.read_file(path, Path::new("")).await? == closure::PAYLOAD);
    }
    drop(store);
    drop(collector);
    CasitaStore::flush_releases().await?;
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let percentile = |p: usize| sorted[(sorted.len() * p).div_ceil(100) - 1];
    println!(
        "{}",
        serde_json::json!({
            "paths": paths_count, "workers": workers, "iterations": iterations, "concurrent_gc": gc,
            "wall_seconds": elapsed, "p50_nanos": percentile(50), "p95_nanos": percentile(95),
            "p99_nanos": percentile(99), "read_nanos": samples, "gc_nanos": collections,
            "profiled_read_phase": profile.is_some(),
            "correctness": "exact payloads, registered paths and shared-DAG references before and after GC"
        })
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 4,
        "usage: casita-retained-reads PATHS WORKERS ITERATIONS GC"
    );
    let (paths, workers, iterations) = (
        args[0].parse::<usize>()?,
        args[1].parse::<usize>()?,
        args[2].parse::<usize>()?,
    );
    ensure!(paths > 0 && workers > 0 && iterations > 0);
    let gc = args[3].parse::<bool>()?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(300),
                run(paths, workers, iterations, gc),
            )
            .await?
        })
}
