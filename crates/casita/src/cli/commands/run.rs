//! Resolve producer-owned root names and supervise a private runnable checkout.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use casita::experimental::{Repository, RepositoryError, RootName, SpillLimits};
use serde::Deserialize;

use super::{Error, workspace};
use crate::cli::{ApplicationExit, RunArgs, usage_error};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct RunConfig {
    default_scope: Option<String>,
    scopes: BTreeMap<String, String>,
}

impl RunConfig {
    pub(super) fn validate(&self) -> Result<(), Error> {
        for (scope, prefix) in &self.scopes {
            if scope.is_empty()
                || !scope
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            {
                return Err(usage_error(format!(
                    "invalid run scope {scope:?}: use letters, digits, '_' or '-'"
                )));
            }
            RootName::try_from(prefix.as_str()).map_err(|error| {
                usage_error(format!("invalid prefix for run scope {scope:?}: {error}"))
            })?;
        }
        if let Some(scope) = &self.default_scope
            && !self.scopes.contains_key(scope)
        {
            return Err(usage_error(format!("unknown default run scope {scope:?}")));
        }
        Ok(())
    }

    fn resolve(&self, input: &str) -> Result<RootName, Error> {
        // A leading slash explicitly escapes both scope and default expansion.
        // Only the first component can contain a scope selector; colons later
        // in a literal root retain their ordinary root-name meaning.
        let resolved = if let Some(literal) = input.strip_prefix('/') {
            literal.to_owned()
        } else if input
            .split('/')
            .next()
            .is_some_and(|part| part.contains(':'))
        {
            let (scope, suffix) = input.split_once(':').expect("scope separator");
            self.expand(scope, suffix)?
        } else if !input.contains('/') {
            match &self.default_scope {
                Some(scope) => self.expand(scope, input)?,
                None => input.to_owned(),
            }
        } else {
            input.to_owned()
        };
        RootName::try_from(resolved)
            .map_err(|error| usage_error(format!("invalid run root: {error}")))
    }

    fn expand(&self, scope: &str, suffix: &str) -> Result<String, Error> {
        let prefix = self.scopes.get(scope)
            .ok_or_else(|| usage_error(format!("unknown run scope {scope:?}; configure [run.scopes] in .casita or supply a full root path")))?;
        RootName::try_from(suffix).map_err(|error| {
            usage_error(format!("invalid name in run scope {scope:?}: {error}"))
        })?;
        Ok(format!("{prefix}/{suffix}"))
    }
}

/// Explicit executables must resolve inside this tree.
/// Stored relative symlinks are supported; links to host executables are not.
fn contained_file(tree: &Path, relative: &Path) -> Result<PathBuf, Error> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(usage_error(
            "run executable must be a relative path within the output tree",
        ));
    }
    let path = std::fs::canonicalize(tree.join(relative))?;
    if !path.starts_with(tree) || !path.is_file() {
        return Err(usage_error(
            "run entry point must resolve to a regular file within the output tree",
        ));
    }
    Ok(tree.join(relative))
}

fn is_executable(path: &Path, metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = path;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Native Windows programs; command scripts would introduce a shell.
        path.extension().is_some_and(|extension| {
            extension.as_encoded_bytes().eq_ignore_ascii_case(b"exe")
                || extension.as_encoded_bytes().eq_ignore_ascii_case(b"com")
        })
    }
}

fn discover_executables(tree: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut directories = vec![PathBuf::new()];
    let mut candidates = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(tree.join(&directory))? {
            let entry = entry?;
            let relative = directory.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(relative);
            } else if kind.is_file() {
                if is_executable(&relative, &entry.metadata()?) {
                    candidates.push(relative);
                }
            } else if kind.is_symlink() {
                // Never descend through directory links: they can create
                // cycles or make one subtree appear under many names. Broken
                // links and links outside this output are not runnable choices.
                if let Ok(target) = std::fs::canonicalize(entry.path())
                    && target.starts_with(tree)
                    && is_executable(&relative, &std::fs::metadata(target)?)
                {
                    candidates.push(relative);
                }
            }
        }
    }
    candidates.sort();
    Ok(candidates)
}

fn matches_bin(path: &Path, name: &Path) -> bool {
    if path.file_name() == Some(name.as_os_str()) {
        return true;
    }
    #[cfg(windows)]
    if path.file_stem() == Some(name.as_os_str()) {
        return true;
    }
    false
}

fn executable(tree: &Path, selection: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(path) = selection
        && (path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_))))
    {
        let file = contained_file(tree, path)?;
        if !is_executable(&file, &std::fs::metadata(&file)?) {
            return Err(usage_error(format!(
                "--bin {} is not executable",
                path.display()
            )));
        }
        return Ok(file);
    }

    let candidates = discover_executables(tree)?;
    let matches: Vec<_> = candidates
        .iter()
        .filter(|path| selection.is_none_or(|name| matches_bin(path, name)))
        .collect();
    if let [only] = matches.as_slice() {
        return Ok(tree.join(only));
    }
    let reason = match (selection, matches.is_empty()) {
        (Some(name), true) => format!("no executable matches --bin {}", name.display()),
        (Some(name), false) => format!(
            "multiple executables match --bin {}; select a relative path",
            name.display()
        ),
        (None, true) => "no executables found in the output tree".to_owned(),
        (None, false) => {
            "multiple executables found; select one with --bin NAME or --bin PATH".to_owned()
        }
    };
    let available = candidates
        .iter()
        .map(|path| format!("\n  {}", path.display()))
        .collect::<String>();
    Err(usage_error(format!("{reason}{available}")))
}

pub(super) async fn execute(
    args: RunArgs,
    repository_dir: &Path,
    spill_limits: SpillLimits,
    pack_target_bytes: Option<u64>,
) -> Result<(), Error> {
    let workspace = workspace::discover()?;
    let config = workspace.map(|workspace| workspace.run).unwrap_or_default();
    let name = config.resolve(&args.root)?;
    let repository = match pack_target_bytes {
        Some(target) => {
            Repository::local_with_pack_options(
                repository_dir,
                casita::experimental::PackOptions {
                    target_size: target,
                    ..Default::default()
                },
            )
            .await?
        }
        None => Repository::local(repository_dir).await?,
    }
    .with_spill_limits(spill_limits);
    let hold = repository.retention_hold().await?;
    let key = hold
        .snapshot()
        .root(&name)
        .await?
        .ok_or_else(|| RepositoryError::Absent(format!("root {name}")))?;
    if key.namespace().as_str() != casita::experimental::DIRECTORY_NAMESPACE {
        return Err(usage_error(format!(
            "run requires a directory root, but {name} points to {}",
            key.namespace()
        )));
    }
    // Transfer the snapshot protection to a closure-scoped reader before
    // releasing the snapshot. A root update or GC cannot change this run, and
    // unrelated repository contents need not stay alive for a long process.
    let retained = repository
        .open_payload(&key)
        .await?
        .ok_or_else(|| RepositoryError::Absent(format!("object {key}")))?;
    drop(hold);

    let runs = repository_dir.join("runs");
    std::fs::create_dir_all(&runs)?;
    let temporary = tempfile::Builder::new().prefix("run-").tempdir_in(runs)?;
    let tree = temporary.path().join("output");
    repository.checkout(&key, &tree).await?;
    if let Err(error) = repository.touch_root(&name, &key).await {
        tracing::debug!(%error, "could not update root access time");
    }
    let tree = std::fs::canonicalize(tree)?;
    let executable = executable(&tree, args.bin.as_deref())?;
    let mut command = tokio::process::Command::new(&executable);
    command
        .args(args.args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let status = supervise(&mut command).await?;
    drop(retained);
    temporary.close()?;
    casita::experimental::flush_repository_leases().await?;
    if status.success() {
        Ok(())
    } else {
        Err(Box::new(ApplicationExit(status)))
    }
}

#[cfg(unix)]
async fn supervise(command: &mut tokio::process::Command) -> Result<ExitStatus, Error> {
    use rustix::process::{Pid, Signal, kill_process};
    use tokio::signal::unix::{SignalKind, signal};

    // Install listeners before spawning so a signal cannot leave a running
    // child behind while Casita unwinds its checkout and retained reader.
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut quit = signal(SignalKind::quit())?;
    let mut child = command.spawn()?;
    let pid =
        Pid::from_raw(child.id().expect("spawned child ID") as i32).expect("positive child ID");
    loop {
        let forwarded = tokio::select! {
            status = child.wait() => return Ok(status?),
            _ = interrupt.recv() => Signal::INT,
            _ = terminate.recv() => Signal::TERM,
            _ = hangup.recv() => Signal::HUP,
            _ = quit.recv() => Signal::QUIT,
        };
        // The child may have exited just before delivery. Waiting above still
        // reaps it and reports its own exit status.
        let _ = kill_process(pid, forwarded);
    }
}

#[cfg(windows)]
async fn supervise(command: &mut tokio::process::Command) -> Result<ExitStatus, Error> {
    // Console children inherit the console and receive its control events.
    // Register before spawning and keep the supervisor alive until the child
    // exits and is reaped, including when Ctrl-Break reaches the console.
    let mut interrupt = tokio::signal::windows::ctrl_c()?;
    let mut break_signal = tokio::signal::windows::ctrl_break()?;
    let mut child = command.spawn()?;
    loop {
        tokio::select! {
            status = child.wait() => return Ok(status?),
            _ = interrupt.recv() => {},
            _ = break_signal.recv() => {},
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn supervise(command: &mut tokio::process::Command) -> Result<ExitStatus, Error> {
    Ok(command.status().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RunConfig {
        toml::from_str(
            "default-scope = 'cargo'\n[scopes]\ncargo = 'cargo/builds'\ngo = 'go/builds'\n",
        )
        .unwrap()
    }

    #[test]
    fn resolution_is_explicit_and_never_searches() {
        let config = config();
        config.validate().unwrap();
        for (input, expected) in [
            ("uv", "cargo/builds/uv"),
            ("cargo:uv", "cargo/builds/uv"),
            ("go:server", "go/builds/server"),
            ("cargo:release/uv", "cargo/builds/release/uv"),
            ("go/builds/server", "go/builds/server"),
            ("cargo/builds/uv", "cargo/builds/uv"),
            ("/uv", "uv"),
            ("/literal:root", "literal:root"),
            ("releases/name:version", "releases/name:version"),
        ] {
            assert_eq!(config.resolve(input).unwrap().as_str(), expected);
        }
        for input in [
            "missing:uv",
            "cargo:",
            "cargo:/uv",
            "cargo:../uv",
            "../uv",
            "cargo//uv",
            "",
        ] {
            assert!(config.resolve(input).is_err(), "{input}");
        }
        assert_eq!(RunConfig::default().resolve("uv").unwrap().as_str(), "uv");
        assert!(RunConfig::default().resolve("cargo:uv").is_err());
    }

    #[test]
    fn invalid_config_does_not_silently_change_resolution() {
        for source in [
            "default-scope = 'missing'",
            "[scopes]\n'bad/name' = 'cargo/builds'",
            "[scopes]\ncargo = 'cargo/../builds'",
        ] {
            let config: RunConfig = toml::from_str(source).unwrap();
            assert!(config.validate().is_err());
        }
        assert!(toml::from_str::<RunConfig>("default_scope = 'cargo'").is_err());
    }

    #[test]
    fn discovery_requires_one_candidate_or_an_unambiguous_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let tree = std::fs::canonicalize(temporary.path()).unwrap();
        assert!(
            executable(&tree, None)
                .unwrap_err()
                .to_string()
                .contains("no executables")
        );
        std::fs::write(tree.join("README"), "not executable").unwrap();
        std::fs::create_dir(tree.join("bin")).unwrap();
        let app = if cfg!(windows) { "app.exe" } else { "app" };
        let nested = Path::new("bin").join(app);
        write_executable(&tree.join(&nested));
        assert_eq!(executable(&tree, None).unwrap(), tree.join(&nested));
        assert_eq!(
            executable(&tree, Some(Path::new(app))).unwrap(),
            tree.join(&nested)
        );
        #[cfg(windows)]
        assert_eq!(
            executable(&tree, Some(Path::new("app"))).unwrap(),
            tree.join(&nested)
        );

        write_executable(&tree.join(app));
        assert!(
            executable(&tree, None)
                .unwrap_err()
                .to_string()
                .contains("--bin")
        );
        assert!(executable(&tree, Some(Path::new(app))).is_err());
        assert_eq!(
            executable(&tree, Some(&nested)).unwrap(),
            tree.join(&nested)
        );
        assert_eq!(
            executable(&tree, Some(&Path::new(".").join(app))).unwrap(),
            tree.join(".").join(app)
        );
        for invalid in ["", "../app", "/app", "README", "missing", "bin"] {
            assert!(
                executable(&tree, Some(Path::new(invalid))).is_err(),
                "{invalid}"
            );
        }
    }

    fn write_executable(path: &Path) {
        std::fs::write(path, "payload").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_discovers_native_programs_without_using_pathext() {
        let temporary = tempfile::tempdir().unwrap();
        for name in [
            "app.EXE",
            "legacy.com",
            "script.cmd",
            "script.bat",
            "library.dll",
            "README",
        ] {
            std::fs::write(temporary.path().join(name), "payload").unwrap();
        }
        assert_eq!(
            discover_executables(temporary.path()).unwrap(),
            vec![PathBuf::from("app.EXE"), PathBuf::from("legacy.com")]
        );
    }
}
