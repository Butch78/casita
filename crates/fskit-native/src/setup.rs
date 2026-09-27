//! Rootless native FSKit registration and activation.
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
fn user_home() -> io::Result<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FSKit setup requires macOS",
        ));
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "run FSKit setup as the ordinary macOS user",
        ));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("FSKit setup requires an absolute HOME directory"))
}

trait Commands {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output>;

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn checked(&self, program: &str, args: &[&OsStr]) -> io::Result<Vec<u8>> {
        let output = self.output(program, args)?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{program} failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }
}

struct SystemCommands;
impl Commands for SystemCommands {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
        Command::new(program)
            .args(args)
            .output()
            .map_err(|error| io::Error::other(format!("cannot run {program}: {error}")))
    }
}

/// A selected installation protected against replacement by this setup API.
/// Keep this handle alive until the associated mount has been unmounted.
pub struct Installation {
    app: PathBuf,
    extension: PathBuf,
    _lock: File,
}

impl Installation {
    pub fn app(&self) -> &Path {
        &self.app
    }

    /// Read signed extension metadata, for application protocol checks.
    pub fn property(&self, key: &str) -> io::Result<Vec<u8>> {
        SystemCommands.checked(
            "/usr/bin/plutil",
            &[
                "-extract".as_ref(),
                key.as_ref(),
                "raw".as_ref(),
                self.extension.join("Contents/Info.plist").as_os_str(),
            ],
        )
    }
}

fn setup_lock(home: &Path) -> io::Result<File> {
    let state = home.join(".local/share/fskit-native");
    fs::create_dir_all(&state)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("setup.lock"))
}

/// Discover the user's selected extension without changing registration
/// or restarting an agent. Multiple processes can hold installations concurrently.
/// macOS checks user approval when mounting. Discovery deliberately does not read
/// its protected activation settings, which may be inaccessible over SSH.
pub fn installed(extension_name: &str, module: &str) -> io::Result<Installation> {
    validate_names(extension_name, module)?;
    let home = user_home()?;
    let lock = setup_lock(&home)?;
    lock.try_lock_shared().map_err(|error| {
        io::Error::other(format!(
            "FSKit setup is in progress; retry mounting: {error}"
        ))
    })?;
    let extension = installed_with(&SystemCommands, &home, extension_name, module)?;
    let app = extension.ancestors().nth(3).unwrap().to_path_buf();
    Ok(Installation {
        app,
        extension,
        _lock: lock,
    })
}

fn installed_with(
    commands: &impl Commands,
    home: &Path,
    extension_name: &str,
    module: &str,
) -> io::Result<PathBuf> {
    let pending = home
        .join(".local/share/fskit-native")
        .join(module)
        .join("activation-pending");
    if pending.try_exists()? {
        return Err(io::Error::other(
            "FSKit activation is incomplete; finish explicit setup before mounting",
        ));
    }
    // Default matching returns the selected version, not every registered copy.
    let output = commands.checked(
        "/usr/bin/pluginkit",
        &[
            "-m".as_ref(),
            "-vv".as_ref(),
            "-i".as_ref(),
            module.as_ref(),
        ],
    )?;
    let paths = parse_registered_paths(&output)?;
    let [extension] = paths.as_slice() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "FSKit needs one selected installation; run explicit setup before mounting",
        ));
    };
    validate_extension_path(extension, extension_name)?;
    let app = extension.ancestors().nth(3).unwrap();
    commands.checked(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ],
    )?;
    let identifier = commands.checked(
        "/usr/bin/plutil",
        &[
            "-extract".as_ref(),
            "CFBundleIdentifier".as_ref(),
            "raw".as_ref(),
            extension.join("Contents/Info.plist").as_os_str(),
        ],
    )?;
    if identifier.trim_ascii() != module.as_bytes() {
        return Err(io::Error::other(
            "selected FSKit bundle identifier differs from the requested module",
        ));
    }
    Ok(extension.clone())
}

fn validate_names(extension_name: &str, module: &str) -> io::Result<()> {
    if !valid_identifier(module)
        || !extension_name.ends_with(".appex")
        || Path::new(extension_name).components().count() != 1
        || extension_name.contains('/')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid extension name or module identifier",
        ));
    }
    Ok(())
}

fn validate_extension_path(path: &Path, extension_name: &str) -> io::Result<()> {
    if !path.is_absolute()
        || path.file_name() != Some(OsStr::new(extension_name))
        || path.parent().and_then(Path::file_name) != Some(OsStr::new("Extensions"))
        || path.ancestors().nth(2).and_then(Path::file_name) != Some(OsStr::new("Contents"))
        || path.ancestors().nth(3).and_then(Path::extension) != Some(OsStr::new("app"))
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::other(
            "unexpected registered FSKit extension path",
        ));
    }
    Ok(())
}

/// Verify and register an app's FSKit extension, activating it where permitted.
///
/// Replacing a registered bundle requires all FSKit volumes to be unmounted.
/// Registration changes leave a durable marker until the required restart succeeds,
/// so an interrupted upgrade can be retried with the same arguments.
/// If activation settings are private, registration and agent restart finish,
/// and registration succeeds without claiming approval. The caller must attempt
/// mounting and report approval guidance if macOS denies it. Other activation
/// errors remain fatal.
pub fn register(app: &Path, extension_name: &str, module: &str) -> io::Result<()> {
    validate_names(extension_name, module)?;
    let app = app.canonicalize()?;
    let home = user_home()?;
    let state = home.join(".local/share/fskit-native");
    let base = state.join(module);
    fs::create_dir_all(&base)?;
    // All modules share one settings array and one per-user FSKit agent.
    let lock = setup_lock(&home)?;
    lock.try_lock().map_err(|error| io::Error::other(format!(
        "FSKit installation is in use by mounts or another setup; retry setup when idle: {error}"
    )))?;
    register_with(&SystemCommands, &home, &base, &app, extension_name, module)
}

fn valid_identifier(module: &str) -> bool {
    !module.is_empty()
        && module != "."
        && module != ".."
        && module
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
}

fn registered_paths(commands: &impl Commands, module: &str) -> io::Result<Vec<PathBuf>> {
    let registered = commands.checked(
        "/usr/bin/pluginkit",
        &[
            "-m".as_ref(),
            "-A".as_ref(),
            "-D".as_ref(),
            "-vv".as_ref(),
            "-i".as_ref(),
            module.as_ref(),
        ],
    )?;
    parse_registered_paths(&registered)
}

fn parse_registered_paths(registered: &[u8]) -> io::Result<Vec<PathBuf>> {
    let text = std::str::from_utf8(registered)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Path = ").map(PathBuf::from))
        .collect())
}

fn require_idle(commands: &impl Commands) -> io::Result<()> {
    let mounts = commands.checked("/sbin/mount", &[])?;
    if has_fskit_mount(&String::from_utf8_lossy(&mounts)) {
        return Err(io::Error::other(
            "unmount existing FSKit volumes before changing registration or restarting its agent",
        ));
    }
    Ok(())
}

fn register_with(
    commands: &impl Commands,
    home: &Path,
    base: &Path,
    app: &Path,
    extension_name: &str,
    module: &str,
) -> io::Result<()> {
    let extension = app.join("Contents/Extensions").join(extension_name);
    commands.checked(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ],
    )?;
    let identifier = commands.checked(
        "/usr/bin/plutil",
        &[
            "-extract".as_ref(),
            "CFBundleIdentifier".as_ref(),
            "raw".as_ref(),
            extension.join("Contents/Info.plist").as_os_str(),
        ],
    )?;
    if String::from_utf8_lossy(&identifier).trim() != module {
        return Err(io::Error::other(
            "FSKit extension bundle identifier differs from the requested module",
        ));
    }
    let paths = registered_paths(commands, module)?;
    let automatic_activation = match read_modules(commands, home) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => false,
        Err(error) => return Err(error),
    };
    if paths != [extension.clone()] {
        require_idle(commands)?;
        for path in &paths {
            validate_extension_path(path, extension_name)?;
        }
        fs::write(
            base.join("activation-pending"),
            b"registration changed; restart required\n",
        )?;
        for path in &paths {
            if path == &extension {
                continue;
            }
            commands.checked("/usr/bin/pluginkit", &["-r".as_ref(), path.as_os_str()])?;
            commands.checked(
                LSREGISTER,
                &["-u".as_ref(), path.ancestors().nth(3).unwrap().as_os_str()],
            )?;
        }
        commands.checked(
            LSREGISTER,
            &[
                "-f".as_ref(),
                "-R".as_ref(),
                "-trusted".as_ref(),
                app.as_os_str(),
            ],
        )?;
        commands.checked(
            "/usr/bin/pluginkit",
            &["-a".as_ref(), extension.as_os_str()],
        )?;
        if registered_paths(commands, module)? != std::slice::from_ref(&extension) {
            return Err(io::Error::other(
                "FSKit registration did not select the requested bundle; retry setup",
            ));
        }
    }
    if automatic_activation {
        match enable_module_id(commands, home, base, module) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error),
        }
    }
    // Finish registration even when activation must be approved in Settings.
    // Keep the durable marker until the old agent has been retired, so a failed
    // restart cannot let discovery reuse an incompatible running extension.
    let pending = base.join("activation-pending");
    if pending.try_exists()? {
        require_idle(commands)?;
        restart_agent(commands, &pending)?;
    }
    // Inaccessible settings do not establish that approval is missing. macOS
    // remains responsible for authorizing the actual mount, just as it is for
    // installed() discovery. Do not turn successful registration into a false
    // activation failure or prevent an already-approved extension from mounting.
    Ok(())
}

fn read_modules(commands: &impl Commands, home: &Path) -> io::Result<Vec<String>> {
    let settings =
        home.join("Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist");
    match commands.read(&settings) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(activation_access_error(&settings, error)),
    }
    let json = commands.checked(
        "/usr/bin/plutil",
        &[
            "-convert".as_ref(),
            "json".as_ref(),
            "-o".as_ref(),
            "-".as_ref(),
            settings.as_os_str(),
        ],
    )?;
    serde_json::from_slice(&json).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn enable_module_id(
    commands: &impl Commands,
    home: &Path,
    base: &Path,
    module: &str,
) -> io::Result<()> {
    let settings =
        home.join("Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist");
    let pending = base.join("activation-pending");
    let modules = read_modules(commands, home)?;
    let enabled = modules.iter().any(|entry| entry == module);
    if enabled && !pending.exists() {
        return Ok(());
    }
    require_idle(commands)?;
    if !enabled {
        let parent = settings.parent().unwrap();
        fs::create_dir_all(parent)?;
        let updated = tempfile::NamedTempFile::new_in(parent)?;
        if settings.exists() {
            fs::copy(&settings, updated.path())?;
            let backup = tempfile::Builder::new()
                .prefix("enabledModules.backup-")
                .tempfile_in(parent)?;
            fs::copy(&settings, backup.path())?;
            let (_, backup) = backup.keep().map_err(|error| error.error)?;
            tracing::info!(path = %backup.display(), "backed up FSKit module settings");
        } else {
            // NamedTempFile already exists. PlistBuddy rejects an existing
            // zero-length file, even for `Clear array`, so seed a valid plist.
            fs::write(updated.path(), b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><array/></plist>\n")?;
        }
        let add = OsString::from(format!("Add : string {module}"));
        commands.checked(
            "/usr/libexec/PlistBuddy",
            &["-c".as_ref(), &add, updated.path().as_os_str()],
        )?;
        // Persist intent first so a failure after updating the plist can retry
        // activation without adding a duplicate module or losing the restart.
        fs::write(&pending, b"restart required\n")?;
        updated.persist(&settings).map_err(|error| error.error)?;
    }
    restart_agent(commands, &pending)
}

fn restart_agent(commands: &impl Commands, pending: &Path) -> io::Result<()> {
    let uid = OsString::from(unsafe { libc::geteuid() }.to_string());
    let agents = commands.output(
        "/usr/bin/pgrep",
        &["-u".as_ref(), &uid, "-x".as_ref(), "fskit_agent".as_ref()],
    )?;
    if !agents.status.success() && agents.status.code() != Some(1) {
        return Err(io::Error::other(
            "cannot enumerate the current user's FSKit agents",
        ));
    }
    for pid in String::from_utf8_lossy(&agents.stdout).split_whitespace() {
        let pid: u32 = pid
            .parse()
            .map_err(|_| io::Error::other("invalid FSKit agent PID"))?;
        if pid <= 1 {
            return Err(io::Error::other("invalid FSKit agent PID"));
        }
        let pid = OsString::from(pid.to_string());
        commands.checked("/bin/kill", &["-KILL".as_ref(), &pid])?;
    }
    if pending.exists() {
        fs::remove_file(pending)?;
    }
    Ok(())
}

fn activation_access_error(settings: &Path, error: io::Error) -> io::Error {
    let guidance = if error.kind() == io::ErrorKind::PermissionDenied {
        " macOS privacy controls can deny access even when you own this file. \
         Run setup from a process authorized to access this container. \
         Manually enabling the extension alone does not grant setup access to these settings. \
         For Tailscale SSH, macOS may attribute access to tailscaled rather than Apple's SSH service. \
         Signing the filesystem extension does not grant the setup process this access."
    } else {
        ""
    };
    io::Error::new(
        error.kind(),
        format!(
            "cannot read FSKit activation settings {}: {error}.{guidance}",
            settings.display()
        ),
    )
}

fn has_fskit_mount(mounts: &str) -> bool {
    mounts
        .lines()
        .filter_map(|line| line.rsplit_once(" (").map(|(_, flags)| flags))
        .any(|flags| {
            flags
                .trim_end_matches(')')
                .split(',')
                .any(|flag| flag.trim() == "fskit")
        })
}

#[cfg(test)]
mod tests;
