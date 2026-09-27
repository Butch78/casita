#![cfg(all(feature = "cli", unix))]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_casita");

struct Fixture {
    _temp: tempfile::TempDir,
    repository: PathBuf,
    project: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(script: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let project = temp.path().join("project");
        let source = temp.path().join("source");
        fs::create_dir_all(project.join("nested")).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join("bin/app"), format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(source.join("bin/app"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(project.join(".casita"), "casita-workspace-v1\nworkspace = \"12345678-1234-4234-8234-123456789abc\"\n\n[run]\ndefault-scope = 'cargo'\n[run.scopes]\ncargo = 'cargo/builds'\ngo = 'go/builds'\n").unwrap();
        Self {
            _temp: temp,
            repository,
            project,
            source,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .arg("--repository")
            .arg(&self.repository)
            .current_dir(self.project.join("nested"));
        command
    }

    fn import(&self, root: &str) {
        success(
            self.command()
                .arg("import")
                .arg(&self.source)
                .args(["--root", root, "--filesystem-rehash"])
                .output()
                .unwrap(),
        );
    }

    fn no_checkouts(&self) {
        if let Ok(mut runs) = fs::read_dir(self.repository.join("runs")) {
            assert!(runs.next().is_none(), "run leaked its temporary checkout");
        }
    }
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn root_retention_cli_persists_policy_and_defaults_to_permanent() {
    let fixture = Fixture::new("exit 0");
    fixture.import("cargo/builds/uv");
    success(
        fixture
            .command()
            .args(["root", "retention", "cargo/builds/uv", "evictable"])
            .output()
            .unwrap(),
    );
    let listed = success(
        fixture
            .command()
            .args(["root", "ls", "--long"])
            .output()
            .unwrap(),
    );
    let listed = String::from_utf8(listed.stdout).unwrap();
    let line = listed
        .lines()
        .find(|line| line.ends_with("cargo/builds/uv"))
        .unwrap();
    assert!(line.contains("  evictable  "));
    let target = line.split_whitespace().next().unwrap();
    success(
        fixture
            .command()
            .args([
                "root",
                "set",
                "releases/current",
                target,
                "--retention",
                "permanent",
            ])
            .output()
            .unwrap(),
    );
    let listed = success(
        fixture
            .command()
            .args(["root", "ls", "--long"])
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .any(|line| line.contains("  permanent  releases/current"))
    );
    success(
        fixture
            .command()
            .args(["root", "retention", "cargo/builds/uv", "permanent"])
            .output()
            .unwrap(),
    );
    let listed = success(
        fixture
            .command()
            .args(["root", "ls", "--long"])
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .any(|line| line.contains("  permanent  cargo/builds/uv"))
    );
    success(
        fixture
            .command()
            .args(["root", "retention", "cargo/builds/uv", "evictable"])
            .output()
            .unwrap(),
    );
    success(
        fixture
            .command()
            .args(["root", "rm", "cargo/builds/uv"])
            .output()
            .unwrap(),
    );
    success(
        fixture
            .command()
            .args(["root", "set", "cargo/builds/uv", target])
            .output()
            .unwrap(),
    );
    let listed = success(
        fixture
            .command()
            .args(["root", "ls", "--long"])
            .output()
            .unwrap(),
    );
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .any(|line| line.contains("  permanent  cargo/builds/uv"))
    );
}

#[test]
fn import_retention_sets_and_preserves_policy() {
    let fixture = Fixture::new("exit 0");
    let import = |retention: Option<&str>| {
        let mut command = fixture.command();
        command.arg("import").arg(&fixture.source).args([
            "--root",
            "cargo/builds/uv",
            "--filesystem-rehash",
        ]);
        if let Some(retention) = retention {
            command.args(["--retention", retention]);
        }
        success(command.output().unwrap());
    };
    let policy = || {
        let listed = success(
            fixture
                .command()
                .args(["root", "ls", "cargo/builds/uv", "--long"])
                .output()
                .unwrap(),
        );
        String::from_utf8(listed.stdout).unwrap()
    };

    import(Some("evictable"));
    assert!(policy().contains("  evictable  cargo/builds/uv"));
    import(None);
    assert!(policy().contains("  evictable  cargo/builds/uv"));
    import(Some("permanent"));
    assert!(policy().contains("  permanent  cargo/builds/uv"));
}

#[test]
fn mixed_scopes_and_literal_roots_preserve_arguments_cwd_and_environment() {
    let fixture = Fixture::new("printf '%s\\n' \"$PWD\" \"$CASITA_RUN_TEST\" \"$@\"");
    fixture.import("cargo/builds/uv");
    fixture.import("go/builds/server");
    for root in [
        "uv",
        "cargo:uv",
        "cargo/builds/uv",
        "go:server",
        "go/builds/server",
    ] {
        let output = success(
            fixture
                .command()
                .env("CASITA_RUN_TEST", "inherited")
                .args([
                    "run",
                    root,
                    "--",
                    "--help",
                    "two words",
                    "",
                    "--log-filter",
                    "literal",
                ])
                .output()
                .unwrap(),
        );
        let cwd = fs::canonicalize(fixture.project.join("nested")).unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "{}\ninherited\n--help\ntwo words\n\n--log-filter\nliteral\n",
                cwd.display()
            )
        );
        fixture.no_checkouts();
    }
}

#[test]
fn selected_binary_and_child_failure_are_not_casita_diagnostics() {
    let fixture = Fixture::new("printf 'child stderr\\n' >&2\nexit 37");
    fixture.import("cargo/builds/uv");
    let output = fixture
        .command()
        .args(["run", "uv", "--bin", "app"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(37));
    assert_eq!(output.stderr, b"child stderr\n");
    assert!(output.stdout.is_empty());
    fixture.no_checkouts();
}

#[test]
fn unknown_names_never_fall_back_to_path_or_other_roots() {
    let fixture = Fixture::new("exit 0");
    fixture.import("uv");
    for root in ["uv", "missing:uv", "sh"] {
        assert!(
            !fixture
                .command()
                .args(["run", root])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    success(fixture.command().args(["run", "/uv"]).output().unwrap());
    fixture.no_checkouts();
}

#[test]
fn nearest_workspace_wins_and_malformed_config_does_not_fall_back() {
    let fixture = Fixture::new("exit 0");
    fixture.import("go/builds/uv");
    let nested = fixture.project.join("nested/.casita");
    fs::write(&nested, "casita-workspace-v1\nworkspace = '87654321-1234-4234-8234-123456789abc'\n[run]\ndefault-scope = 'go'\n[run.scopes]\ngo = 'go/builds'\n").unwrap();
    success(fixture.command().args(["run", "uv"]).output().unwrap());
    fs::write(nested, "broken marker").unwrap();
    assert!(
        !fixture
            .command()
            .args(["run", "uv"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn discovery_ignores_external_links_and_internal_file_symlinks_are_selectable() {
    let fixture = Fixture::new("printf 'inside\\n'");
    symlink("/bin/sh", fixture.source.join("bin/escape")).unwrap();
    symlink("missing", fixture.source.join("broken")).unwrap();
    symlink(".", fixture.source.join("cycle")).unwrap();
    symlink("bin", fixture.source.join("alias-dir")).unwrap();
    fixture.import("cargo/builds/uv");
    // Directory aliases and cycles are not traversed, and external and broken
    // file links are ignored, leaving only the real bin/app executable.
    let automatic = success(fixture.command().args(["run", "uv"]).output().unwrap());
    assert_eq!(automatic.stdout, b"inside\n");
    for path in ["/bin/sh", "../outside", "bin/escape", "bin", "bin/missing"] {
        assert!(
            !fixture
                .command()
                .args(["run", "uv", "--bin", path])
                .output()
                .unwrap()
                .status
                .success(),
            "{path}"
        );
        fixture.no_checkouts();
    }
    symlink("app", fixture.source.join("bin/alias")).unwrap();
    fixture.import("cargo/builds/uv");
    let ambiguous = fixture.command().args(["run", "uv"]).output().unwrap();
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("--bin"));
    let output = success(
        fixture
            .command()
            .args(["run", "uv", "--bin", "alias"])
            .output()
            .unwrap(),
    );
    assert_eq!(output.stdout, b"inside\n");
    fixture.no_checkouts();
}

#[test]
fn multiple_binaries_require_selection_and_duplicate_names_require_paths() {
    let fixture = Fixture::new("printf 'app\\n'");
    fs::write(
        fixture.source.join("helper"),
        "#!/bin/sh\nprintf 'helper\\n'\n",
    )
    .unwrap();
    fs::set_permissions(
        fixture.source.join("helper"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(fixture.source.join("README"), "not executable").unwrap();
    fixture.import("cargo/builds/uv");
    let output = fixture.command().args(["run", "uv"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("--bin"));
    assert!(error.contains("\n  bin/app\n  helper"));
    assert!(!error.contains("README"));
    for (bin, expected) in [("app", "app\n"), ("helper", "helper\n")] {
        let output = success(
            fixture
                .command()
                .args(["run", "uv", "--bin", bin])
                .output()
                .unwrap(),
        );
        assert_eq!(output.stdout, expected.as_bytes());
    }
    fs::create_dir(fixture.source.join("debug")).unwrap();
    fs::copy(
        fixture.source.join("helper"),
        fixture.source.join("debug/app"),
    )
    .unwrap();
    fixture.import("cargo/builds/uv");
    let ambiguous = fixture
        .command()
        .args(["run", "uv", "--bin", "app"])
        .output()
        .unwrap();
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("relative path"));
    let output = success(
        fixture
            .command()
            .args(["run", "uv", "--bin", "debug/app"])
            .output()
            .unwrap(),
    );
    assert_eq!(output.stdout, b"helper\n");
    let unknown = fixture
        .command()
        .args(["run", "uv", "--bin", "missing"])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no executable matches"));
    fixture.no_checkouts();
}

#[test]
fn outputs_without_executables_fail_without_launching_anything() {
    let fixture = Fixture::new("printf 'must not run\\n'");
    fs::set_permissions(
        fixture.source.join("bin/app"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fixture.import("cargo/builds/uv");
    let output = fixture.command().args(["run", "uv"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no executables found"));
    for bin in ["app", "bin/app"] {
        let output = fixture
            .command()
            .args(["run", "uv", "--bin", bin])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    fixture.no_checkouts();
}

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "child did not create {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn root_replacement_and_collection_do_not_change_a_running_output() {
    let fixture = Fixture::new(
        "touch ready\nwhile [ ! -f continue ]; do sleep 0.02; done\ncat \"$(dirname \"$0\")/../data\" > observed",
    );
    fs::write(fixture.source.join("data"), "original").unwrap();
    fixture.import("cargo/builds/uv");
    let mut running = Running(
        fixture
            .command()
            .args(["run", "uv"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let cwd = fixture.project.join("nested");
    wait_for_file(&cwd.join("ready"));
    fs::write(fixture.source.join("data"), "replacement").unwrap();
    fixture.import("cargo/builds/uv");
    success(
        fixture
            .command()
            .args(["root", "rm", "cargo/builds/uv"])
            .output()
            .unwrap(),
    );
    success(fixture.command().arg("gc").output().unwrap());
    fs::write(cwd.join("continue"), "").unwrap();
    assert!(wait_for_exit(&mut running.0).success());
    assert_eq!(
        fs::read_to_string(cwd.join("observed")).unwrap(),
        "original"
    );
    fixture.no_checkouts();
}

#[test]
fn signals_sent_to_casita_reach_the_child_and_cleanup_finishes() {
    use rustix::process::{Pid, Signal, kill_process};
    let fixture = Fixture::new("trap 'exit 23' TERM\ntouch ready\nwhile :; do sleep 0.02; done");
    fixture.import("cargo/builds/uv");
    let mut running = Running(
        fixture
            .command()
            .args(["run", "uv"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for_file(&fixture.project.join("nested/ready"));
    kill_process(Pid::from_raw(running.0.id() as i32).unwrap(), Signal::TERM).unwrap();
    assert_eq!(wait_for_exit(&mut running.0).code(), Some(23));
    fixture.no_checkouts();
}

#[test]
fn standard_input_and_signal_exit_status_are_preserved() {
    use std::io::Write;
    let fixture = Fixture::new("cat\nkill -TERM $$");
    fixture.import("cargo/builds/uv");
    let mut child = fixture
        .command()
        .args(["run", "uv"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"input bytes\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.stdout, b"input bytes\n");
    assert!(output.stderr.is_empty());
    assert_eq!(output.status.code(), Some(143));
    fixture.no_checkouts();
}
