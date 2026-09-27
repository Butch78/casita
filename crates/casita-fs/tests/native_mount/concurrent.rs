use anyhow::{ensure, Context, Result};
use casita_fs::{
    darwin::{native_protocol, setup, PersistentMount},
    ContentReader,
};
use std::{
    fs,
    io::BufRead,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires macOS 26+ and an installed, enabled Casita extension"]
fn independent_processes() -> Result<()> {
    // Fail before creating workers if explicit setup is needed.
    let bundle = setup::installed_native()?.app().to_owned();
    let before = SharedState::read()?;
    let mut work = tempfile::tempdir()?;
    // A failed worker may still own a mount. Never recursively delete its files.
    work.disable_cleanup(true);
    eprintln!("concurrent mount fixtures: {}", work.path().display());
    let mut first = Worker::start(&work.path().join("first"), "first")?;
    let mut second = Worker::start(&work.path().join("second"), "second")?;
    let first_mount = first.ready()?;
    let second_mount = second.ready()?;
    ensure!(first_mount != second_mount);
    super::verify_extension(&first_mount, &bundle)?;
    super::verify_extension(&second_mount, &bundle)?;
    check(&first_mount, "first", 0)?;
    check(&second_mount, "second", 0)?;
    before.verify()?;

    first.close()?;
    ensure!(!first
        .directory
        .join("repository")
        .join(native_protocol::ACTIVE)
        .exists());
    check(&second_mount, "second", 1)?;
    // Reuse the first repository while the second process remains mounted.
    let mut reopened = Worker::start(&first.directory, "first")?;
    let reopened_mount = reopened.ready()?;
    check(&reopened_mount, "first", 0)?;
    check(&second_mount, "second", 2)?;
    reopened.close()?;
    check(&second_mount, "second", 3)?;
    second.close()?;
    ensure!(!second
        .directory
        .join("repository")
        .join(native_protocol::ACTIVE)
        .exists());
    before.verify()?;
    work.disable_cleanup(false);
    Ok(())
}

fn check(mount: &Path, expected: &str, phase: u8) -> Result<()> {
    // Read a fresh inode each phase so cached file pages cannot conceal a dead mount.
    ensure!(fs::read(mount.join(format!("views/fixture/data-{phase}")))? == expected.as_bytes());
    Ok(())
}

// This test entry point doubles as the child executable. No bundle path is
// passed to the workers; they discover the same installation from different cwd's.
#[test]
#[ignore = "helper launched by independent_processes"]
fn worker() -> Result<()> {
    let Some(directory) = std::env::var_os("CASITA_FSKIT_TEST_WORKER") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    let contents = std::env::var("CASITA_FSKIT_TEST_CONTENTS")?;
    let source = directory.join("source");
    fs::create_dir_all(&source)?;
    for phase in 0..4 {
        fs::write(source.join(format!("data-{phase}")), contents.as_bytes())?;
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let repo_path = directory.join("repository");
    let repository = runtime.block_on(casita::Repository::local(&repo_path))?;
    let root = runtime.block_on(async {
        let key = repository
            .import(casita::import::FilesystemImport::new(
                &source,
                casita::RootName::try_from("fixture")?,
            ))
            .await?;
        let digest = casita::DirectoryId::new(key.native_digest().unwrap());
        let node = casita::Node::Directory {
            digest,
            size: repository.directory(&digest).await?.unwrap().size(),
        };
        repository.flush().await?;
        anyhow::Ok(node)
    })?;
    let mut mount = PersistentMount::new(&repo_path, &directory)?;
    mount.publish_root(b"fixture", root)?;
    check(mount.path(), &contents, 0)?;
    let staged = directory.join("ready.tmp");
    fs::write(&staged, serde_json::to_vec(mount.path())?)?;
    fs::rename(staged, directory.join("ready"))?;
    // EOF from the parent requests a graceful unmount, including on test failure.
    let mut command = String::new();
    std::io::stdin().lock().read_line(&mut command)?;
    mount.close()?;
    runtime.block_on(repository.flush())?;
    Ok(())
}

struct Worker {
    child: Child,
    directory: PathBuf,
}

impl Worker {
    fn start(directory: &Path, contents: &str) -> Result<Self> {
        fs::create_dir_all(directory)?;
        match fs::remove_file(directory.join("ready")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let log = fs::File::create(directory.join("worker.log"))?;
        let child = Command::new(std::env::current_exe()?)
            .args(["--exact", "concurrent::worker", "--ignored", "--nocapture"])
            .current_dir(directory)
            .env("CASITA_FSKIT_TEST_WORKER", directory)
            .env("CASITA_FSKIT_TEST_CONTENTS", contents)
            .env_remove("CASITA_FSKIT_APP")
            .env_remove("CASITA_FSKIT_APP_BUNDLE")
            .stdin(Stdio::piped())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        Ok(Self {
            child,
            directory: directory.to_owned(),
        })
    }

    fn ready(&mut self) -> Result<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::bail!("worker exited {status}: {}", self.log());
            }
            if self.directory.join("ready").try_exists()? {
                return Ok(serde_json::from_slice(&fs::read(
                    self.directory.join("ready"),
                )?)?);
            }
            ensure!(
                Instant::now() < deadline,
                "worker startup timed out: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn close(&mut self) -> Result<()> {
        self.child.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "worker exited {status}: {}", self.log());
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "worker shutdown timed out: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.directory.join("worker.log")).unwrap_or_default()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!(
                "{error:#}; retaining fixtures at {}",
                self.directory.display()
            );
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct SharedState {
    registration: Vec<u8>,
    settings: Vec<u8>,
    agents: Vec<u8>,
    mounts: Vec<String>,
}

impl SharedState {
    fn read() -> Result<Self> {
        let registration = output(
            "/usr/bin/pluginkit",
            &["-m", "-A", "-D", "-vv", "-i", "org.casita.fskit.extension"],
        )?;
        let settings = fs::read(
            Path::new(&std::env::var_os("HOME").context("HOME missing")?).join(
                "Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist",
            ),
        )?;
        let agents = output(
            "/usr/bin/pgrep",
            &[
                "-u",
                &unsafe { libc::geteuid() }.to_string(),
                "-x",
                "fskit_agent",
            ],
        )?;
        let mounts = String::from_utf8(output("/sbin/mount", &[])?)?
            .lines()
            .filter(|line| line.contains("fskit"))
            .map(str::to_owned)
            .collect();
        Ok(Self {
            registration,
            settings,
            agents,
            mounts,
        })
    }

    fn verify(&self) -> Result<()> {
        let after = Self::read()?;
        ensure!(
            self.registration == after.registration,
            "extension registration changed"
        );
        ensure!(self.settings == after.settings, "module enablement changed");
        ensure!(self.agents == after.agents, "FSKit agent restarted");
        for mount in &self.mounts {
            ensure!(
                after.mounts.contains(mount),
                "pre-existing mount disappeared: {mount}"
            );
        }
        Ok(())
    }
}

fn output(program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program).args(args).output()?;
    ensure!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
