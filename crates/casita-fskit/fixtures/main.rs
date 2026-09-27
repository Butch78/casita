use anyhow::{ensure, Result};
use casita_fskit::repository::{self, Backend};
use serde_json::json;
use std::{
    io::{self, Write},
    path::Path,
    time::Instant,
};
fn emit(value: serde_json::Value) -> Result<()> {
    println!("{value}");
    io::stdout().flush()?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(unsafe { libc::geteuid() } != 0, "run without root");
    if args.first().is_some_and(|s| s == "reader-cache") {
        ensure!(args.len() == 4, "reader-cache REPOSITORY WIDTH CYCLES");
        let width: usize = args[2].parse()?;
        let cycles: usize = args[3].parse()?;
        ensure!(
            (15..=17).contains(&width) && (2..=100).contains(&cycles),
            "reader-cache bounds"
        );
        let backend = Backend::open(Path::new(&args[1]))?;
        let fixture = casita_fskit::fixture();
        let mut names: Vec<_> = casita_fskit::SIZES
            .iter()
            .filter(|size| **size > 0)
            .map(|size| format!("size-{size}").into_bytes())
            .collect();
        names.push(b"run".to_vec());
        let mut files = Vec::new();
        for name in names.into_iter().take(width) {
            let entry = backend.lookup(4, &name)?.expect("missing boundary fixture");
            let expected = &fixture
                .iter()
                .find(|item| item.name == name)
                .expect("missing byte oracle")
                .data;
            files.push((entry.id, expected));
        }
        let before = backend.stats();
        let started = Instant::now();
        for cycle in 0..cycles {
            for (id, expected) in &files {
                let offset = (cycle * 7) % expected.len();
                let length = 32.min(expected.len() - offset);
                ensure!(
                    backend.read(*id, offset as u64, 32)? == expected[offset..offset + length],
                    "reader-cache bytes differ"
                );
            }
        }
        let elapsed_ns = started.elapsed().as_nanos() as u64;
        let after = backend.stats();
        let cached = after["native_reader_cache"]["enabled"].as_bool().unwrap();
        let expected_opens = if cached
            && width <= after["native_reader_cache"]["capacity"].as_u64().unwrap() as usize
        {
            width
        } else {
            width * cycles
        };
        ensure!(
            after["blob_opens"].as_u64().unwrap() - before["blob_opens"].as_u64().unwrap()
                == expected_opens as u64,
            "reader-cache retention/eviction differs"
        );
        backend.flush()?;
        ensure!(
            backend.stats()["native_reader_cache"]["resident"] == 0,
            "reader cache retained holds after flush"
        );
        return emit(
            json!({"width":width, "cycles":cycles, "elapsed_ns":elapsed_ns,
                           "before":before, "after":after, "correctness":"passed", "repository_release_barrier":"passed"}),
        );
    }
    if args.first().is_some_and(|s| s == "import") {
        ensure!(args.len() == 3, "import SOURCE REPOSITORY");
        return emit(serde_json::to_value(repository::import(
            Path::new(&args[1]),
            Path::new(&args[2]),
            "fixture",
            "snapshot.json",
        )?)?);
    }
    if args.first().is_some_and(|s| s == "stage") {
        ensure!(
            args.len() == 4
                && args[3]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "stage SOURCE REPOSITORY NAME"
        );
        return emit(serde_json::to_value(repository::import(
            Path::new(&args[1]),
            Path::new(&args[2]),
            &args[3],
            &format!("stage-{}.json", args[3]),
        )?)?);
    }
    anyhow::bail!("expected import, stage, or reader-cache command")
}
