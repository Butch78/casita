//! Test-only timings around the production native import path.
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) const PHASES: [&str; 12] = [
    "mutation_start",
    "existing_view",
    "source_header",
    "source_decode",
    "stage_poll",
    "verify_sum",
    "upload_sum",
    "checkpoint_drain",
    "checkpoint_publish",
    "pack_cache",
    "root_publish",
    "compact",
];

#[derive(Default, Clone, serde::Serialize)]
pub(crate) struct Profile {
    pub calls: [u64; 12],
    pub nanos: [u64; 12],
    pub decoded_objects: u64,
    pub decoded_bytes: u64,
    pub peak_active: usize,
    pub peak_buffered_bytes: u64,
}

tokio::task_local! {
    static CURRENT: Arc<Mutex<Profile>>;
}

pub(crate) async fn capture<T>(future: impl std::future::Future<Output = T>) -> (T, Profile) {
    let profile = Arc::new(Mutex::new(Profile::default()));
    let result = CURRENT.scope(profile.clone(), future).await;
    let profile = profile.lock().unwrap().clone();
    (result, profile)
}

pub(crate) struct Timer {
    profile: Arc<Mutex<Profile>>,
    phase: usize,
    start: Instant,
}
impl Drop for Timer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let mut profile = self.profile.lock().unwrap();
        profile.calls[self.phase] += 1;
        profile.nanos[self.phase] += elapsed;
    }
}

pub(crate) fn time(phase: usize) -> Option<Timer> {
    CURRENT
        .try_with(|profile| Timer {
            profile: profile.clone(),
            phase,
            start: Instant::now(),
        })
        .ok()
}

pub(crate) fn admitted(size: u64, active: usize, buffered: u64) {
    let _ = CURRENT.try_with(|profile| {
        let mut profile = profile.lock().unwrap();
        profile.decoded_objects += 1;
        profile.decoded_bytes += size;
        profile.peak_active = profile.peak_active.max(active);
        profile.peak_buffered_bytes = profile.peak_buffered_bytes.max(buffered);
    });
}

fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmHWM:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
                    .map(|kb| kb * 1024)
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[tokio::test]
#[ignore = "run through benchmark run git-import-profile"]
async fn benchmark_git_import_profile() {
    use super::{NativeGitImportOptions, read_git_view};
    use crate::repository::{ClosureStatus, PUBLICATION_PHASES, Repository};
    let source = std::env::var("CASITA_GIT_SOURCE").unwrap();
    let destination = std::env::var("CASITA_GIT_DESTINATION").unwrap();
    let inventory = std::env::var("CASITA_GIT_INVENTORY").unwrap();
    let tip: crate::ObjectKey = std::env::var("CASITA_GIT_TIP").unwrap().parse().unwrap();
    let operation = std::env::var("CASITA_GIT_OPERATION").unwrap();
    let repository = Repository::local(destination).await.unwrap();
    let options = NativeGitImportOptions {
        view_name: "scale".into(),
        max_cached_pack_bytes: 0,
        ..Default::default()
    };
    let before = repository.publication_profile();
    let start = Instant::now();
    let (result, profile) = capture(repository.import_native_git_view(source, &options)).await;
    let wall_nanos = start.elapsed().as_nanos() as u64;
    let peak_rss_bytes = peak_rss_bytes();
    let after = repository.publication_profile();
    let result = result.unwrap();
    eprintln!(
        "import completed in {:.3}s; auditing {} objects",
        wall_nanos as f64 / 1e9,
        result.objects
    );
    let publication: Vec<_> = PUBLICATION_PHASES.iter().enumerate().map(|(i, name)| serde_json::json!({
        "phase": name, "calls": after.calls[i] - before.calls[i], "nanos": after.nanos[i] - before.nanos[i],
    })).collect();
    // Audit after capturing RSS, so the independent expected inventory and
    // full closure audit do not inflate the reported import peak.
    let expected: std::collections::BTreeSet<crate::ObjectKey> = std::fs::read_to_string(inventory)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    let (key, view) = read_git_view(&repository, "scale").await.unwrap().unwrap();
    assert_eq!(key, result.view);
    assert_eq!(*view.objects(), expected);
    assert_eq!(result.objects, expected.len());
    assert_eq!(
        view.refs
            .get(&crate::CanonicalRefName::try_from("refs/heads/main").unwrap()),
        Some(&crate::GitRefValue::Direct(tip))
    );
    assert!(matches!(
        repository.verify_closure(&key).await.unwrap(),
        ClosureStatus::Complete { .. }
    ));
    println!(
        "git_import_profile {}",
        serde_json::json!({
            "operation": operation, "objects": result.objects, "root": key.to_string(), "wall_nanos": wall_nanos,
            "concurrency": options.concurrency.get(), "max_buffered_bytes": options.max_buffered_bytes.get(),
            "peak_rss_at_import_end_bytes": peak_rss_bytes,
            "phase_names": PHASES, "profile": profile, "publication": publication,
            "correctness": "exact independent Git inventory and ref; persisted view identity; complete verified closure",
        })
    );
}
