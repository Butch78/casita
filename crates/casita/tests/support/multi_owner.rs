//! Shared harness for the multi-owner S3 scenarios: one RustFS server, one
//! S3 prefix per scenario, and every phase in its own child process. Each
//! integration test is its own crate and uses only what it needs.
#![allow(dead_code)]

#[path = "rustfs.rs"]
pub mod rustfs;
#[path = "worker.rs"]
pub mod worker;

use std::path::{Path, PathBuf};
use std::time::Duration;

use casita::{ObjectKey, Repository, RootName};
use tokio::io::AsyncReadExt;

pub const PHASE: &str = "CASITA_S3_MULTI_OWNER_PHASE";
pub const WORK: &str = "CASITA_S3_MULTI_OWNER_WORK";
pub const PREFIX: &str = "CASITA_S3_MULTI_OWNER_PREFIX";

/// One-shot phases finish well within this; it only bounds a hung child.
pub const PHASE_DEADLINE: Duration = Duration::from_secs(90);
/// Long-lived phases wait for the parent between steps.
pub const RENDEZVOUS_DEADLINE: Duration = Duration::from_secs(150);

pub fn name(value: &str) -> RootName {
    value.try_into().unwrap()
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// Deterministic bytes span several chunks without collapsing under compression.
fn bytes(mut state: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// Several content-defined chunks, so the payload has a physical manifest.
pub fn payload(seed: u64) -> Vec<u8> {
    bytes(seed, 3 * 1024 * 1024 + 127)
}

/// Around one chunk; cheap enough for publication loops.
pub fn small(seed: u64) -> Vec<u8> {
    bytes(seed.wrapping_add(1 << 32), 256 * 1024 + 3)
}

/// Offset and replacement used to derive one owner's blob from another's so
/// the two closures differ while sharing every untouched chunk.
pub const OVERWRITE_OFFSET: u64 = 1024 * 1024 + 17;
pub const OVERWRITE_LEN: usize = 4096;

pub fn overwritten(seed: u64, replacement_seed: u64) -> Vec<u8> {
    let mut expected = payload(seed);
    let start = OVERWRITE_OFFSET as usize;
    expected[start..start + OVERWRITE_LEN]
        .copy_from_slice(&small(replacement_seed)[..OVERWRITE_LEN]);
    expected
}

/// Phases hand values to each other through files in the work directory.
pub fn write_value<T: std::fmt::Display + ?Sized>(work: &Path, file: &str, value: &T) {
    std::fs::write(work.join(file), value.to_string()).unwrap();
}

pub fn read_value<T: std::str::FromStr>(work: &Path, file: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    std::fs::read_to_string(work.join(file))
        .unwrap()
        .parse()
        .unwrap()
}

pub fn write_key(work: &Path, file: &str, key: &ObjectKey) {
    write_value(work, file, key);
}

pub fn read_key(work: &Path, file: &str) -> ObjectKey {
    read_value(work, file)
}

pub fn write_count(work: &Path, file: &str, count: usize) {
    write_value(work, file, &count);
}

pub fn read_count(work: &Path, file: &str) -> usize {
    read_value(work, file)
}

/// Poll for a rendezvous file created by the parent.
pub async fn wait_for(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// One scenario owns its S3 prefix and work directory so collection counts
/// are exact and scenarios cannot leak garbage into each other.
pub struct Scenario<'a> {
    fixture: &'a rustfs::Rustfs,
    dispatcher: &'static str,
    work: PathBuf,
    prefix: String,
}

impl<'a> Scenario<'a> {
    /// `dispatcher` names the `#[test]` that runs phases in child processes.
    pub fn new(
        fixture: &'a rustfs::Rustfs,
        root: &Path,
        dispatcher: &'static str,
        scenario: &str,
    ) -> Self {
        let work = root.join(scenario);
        std::fs::create_dir_all(&work).unwrap();
        Self {
            fixture,
            dispatcher,
            work,
            prefix: format!("multi-owner/{scenario}"),
        }
    }

    pub fn worker(&self, phase: &str) -> worker::Worker {
        worker::Worker::new(&self.work, self.dispatcher, phase, |command| {
            self.fixture.configure(command)
        })
        .env(PHASE, phase)
        .env(WORK, &self.work)
        .env(PREFIX, &self.prefix)
    }

    pub fn run(&self, phase: &str) {
        self.worker(phase).spawn().wait(PHASE_DEADLINE);
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }
}

/// What a child phase knows: its work directory and its scenario's prefix.
pub struct Context {
    pub work: PathBuf,
    pub prefix: String,
}

impl Context {
    /// Read the phase selection from the environment; `None` in the parent.
    pub fn from_env() -> Option<(Self, String, String)> {
        let phase = std::env::var(PHASE).ok()?;
        let work = PathBuf::from(std::env::var_os(WORK).unwrap());
        let prefix = std::env::var(PREFIX).unwrap();
        let scenario = prefix.rsplit('/').next().unwrap().to_string();
        Some((Self { work, prefix }, scenario, phase))
    }

    pub async fn remote(&self, writer: &str) -> Repository {
        Repository::s3(rustfs::BUCKET, &self.prefix, writer)
            .await
            .unwrap()
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }
}

/// Two retained readers in separate processes protect one owner's content in
/// turn. Reader A arms first, reader B arms on the same content, A releases
/// and its release settles, `between` runs while only B protects the data,
/// then B verifies the content is still readable and releases. The `publish`
/// and `final` phases bracket the scenario. Phases use
/// [`replacement_publish`], [`replacement_reader`] and [`replacement_final`].
pub fn run_replacement(scenario: &Scenario, between: &str) {
    scenario.run("publish");
    let mut reader_a = scenario.worker("reader-a").spawn();
    worker::await_file(
        &scenario.file("a-ready"),
        &mut [&mut reader_a],
        RENDEZVOUS_DEADLINE,
    );
    let mut reader_b = scenario.worker("reader-b").spawn();
    worker::await_file(
        &scenario.file("b-ready"),
        &mut [&mut reader_a, &mut reader_b],
        RENDEZVOUS_DEADLINE,
    );
    worker::signal(&scenario.file("a-release"));
    reader_a.wait(RENDEZVOUS_DEADLINE);
    scenario.run(between);
    worker::signal(&scenario.file("b-check"));
    worker::await_file(
        &scenario.file("b-done"),
        &mut [&mut reader_b],
        RENDEZVOUS_DEADLINE,
    );
    worker::signal(&scenario.file("b-release"));
    reader_b.wait(RENDEZVOUS_DEADLINE);
    scenario.run("final");
}

/// Owner A imports `payload(seed)` as `owner-a/current` and records its key.
pub async fn replacement_publish(context: &Context, seed: u64) {
    let owner_a = context.remote("owner-a").await;
    let key = owner_a
        .import(casita::import::BlobImport::new(
            &payload(seed)[..],
            name("owner-a/current"),
        ))
        .await
        .unwrap();
    write_key(&context.work, "key", &key);
    owner_a.flush().await.unwrap();
}

/// Reader `owner` retains the published content until the parent releases
/// it. `armed` runs once this process's protection is durable, before the
/// parent is told the reader is ready.
pub async fn replacement_reader(
    context: &Context,
    owner: &str,
    seed: u64,
    armed: impl AsyncFnOnce(),
) {
    let key = read_key(&context.work, "key");
    let writer = format!("reader-{owner}");
    let repository = context.remote(&writer).await;
    let session = repository.retained_reader().await.unwrap();
    let mut reader = session.open(&key).await.unwrap().unwrap();
    drop(repository);
    armed().await;
    worker::signal(&context.file(&format!("{owner}-ready")));
    if owner == "b" {
        wait_for(&context.file("b-check")).await;
        assert_eq!(
            session.root(&name("owner-a/current")).await.unwrap(),
            Some(key.clone())
        );
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, payload(seed));
        worker::signal(&context.file("b-done"));
    }
    wait_for(&context.file(&format!("{owner}-release"))).await;
    drop(reader);
    drop(session);
    context
        .remote(&format!("{writer}-flush"))
        .await
        .flush()
        .await
        .unwrap();
}

/// With both readers released, one collection reclaims the removed content
/// and leaves the repository clean.
pub async fn replacement_final(context: &Context) {
    let key = read_key(&context.work, "key");
    let collector = context.remote("collector").await;
    assert_eq!(collector.collect().await.unwrap().logical_objects, 1);
    assert!(collector.open(&key).await.unwrap().is_none());
    assert!(collector.fsck().await.unwrap().is_clean());
    collector.flush().await.unwrap();
}

pub async fn read_verified(repository: &Repository, key: &ObjectKey) -> Vec<u8> {
    let mut reader = repository
        .open_verified(key)
        .await
        .unwrap()
        .expect("object must still be readable");
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).await.unwrap();
    actual
}
