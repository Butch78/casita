//! Child-process helpers for tests that split one scenario across several
//! processes. Phases rendezvous through files created atomically, and the
//! parent waits on process exits, so no assertion depends on elapsed time.
//! Each integration test is its own crate and uses only the entry points it
//! needs, so unused items are expected here.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A child phase of the current test binary, selected by test name and
/// configured through environment variables before it is spawned.
pub struct Worker {
    command: Command,
    log: PathBuf,
    phase: String,
}

impl Worker {
    /// Re-run this test binary for exactly one `#[test]` named `test`, logging
    /// its output to `<work>/<phase>.log`. `configure` supplies credentials
    /// and other environment that must never leak into the parent process.
    pub fn new(work: &Path, test: &str, phase: &str, configure: impl FnOnce(&mut Command)) -> Self {
        let log = work.join(format!("{phase}.log"));
        let file = std::fs::File::create(&log).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([test, "--exact", "--nocapture"])
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file));
        configure(&mut command);
        Self {
            command,
            log,
            phase: phase.to_string(),
        }
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn spawn(mut self) -> Running {
        Running {
            child: self.command.spawn().unwrap(),
            log: self.log,
            phase: self.phase,
        }
    }
}

/// A spawned phase. Dropping it kills the child so a failing parent never
/// leaves workers behind.
pub struct Running {
    child: Child,
    log: PathBuf,
    phase: String,
}

impl Running {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Fail if the phase exited before the parent expected it to.
    pub fn assert_alive(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "{} exited early: {}",
            self.phase,
            self.log()
        );
    }

    /// Wait for a successful exit. The deadline only bounds a hung test.
    pub fn wait(mut self, deadline: Duration) {
        let deadline = Instant::now() + deadline;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{} failed: {}", self.phase, self.log());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{} timed out: {}",
                self.phase,
                self.log()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Simulate a crash: the phase must still be running when it is killed.
    pub fn kill(mut self) {
        self.assert_alive();
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        assert!(!status.success(), "{} exited cleanly", self.phase);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Block until a child creates `path`, failing early if any child exits.
pub fn await_file(path: &Path, running: &mut [&mut Running], deadline: Duration) {
    let deadline = Instant::now() + deadline;
    while !path.exists() {
        for child in running.iter_mut() {
            child.assert_alive();
        }
        assert!(
            Instant::now() < deadline,
            "{} was never created",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Create a rendezvous file atomically so a waiter never sees a partial write.
pub fn signal(path: &Path) {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, b"").unwrap();
    std::fs::rename(temporary, path).unwrap();
}
