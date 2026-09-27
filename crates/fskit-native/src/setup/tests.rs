use super::*;
const MODULE: &str = "org.example.filesystem";
use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;

struct ReadOnlyCommands(RegistrationCommands);

impl Commands for ReadOnlyCommands {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
        match program {
            "/usr/bin/pluginkit" => assert_eq!(args, ["-m", "-vv", "-i", MODULE]),
            "/usr/bin/codesign" => assert_eq!(args[0], "--verify"),
            "/usr/bin/plutil" => assert_eq!(args[0], "-extract"),
            _ => panic!("mount discovery invoked a mutating or unexpected command: {program}"),
        }
        self.0.output(program, args)
    }
}

#[test]
fn installed_discovery_is_read_only_with_unrelated_mounts() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE, "com.example.other"]);
    let commands = ReadOnlyCommands(registration("resource on /other (local, fskit)\n"));
    let extension = PathBuf::from("/shared/Installed.app/Contents/Extensions/test.appex");
    *commands.0.paths.borrow_mut() = vec![extension.clone()];
    let before = fs::read(settings(home.path())).unwrap();
    for _ in 0..2 {
        assert_eq!(
            installed_with(&commands, home.path(), "test.appex", MODULE).unwrap(),
            extension
        );
    }
    assert_eq!(fs::read(settings(home.path())).unwrap(), before);
    assert!(commands.0.mutations.borrow().is_empty());
}

#[test]
fn installed_discovery_rejects_incomplete_invalid_or_ambiguous_state() {
    let home = tempfile::tempdir().unwrap();
    let commands = ReadOnlyCommands(registration(""));
    let extension = PathBuf::from("/shared/Installed.app/Contents/Extensions/test.appex");
    seed_settings(home.path(), &[MODULE]);
    commands.0.paths.borrow_mut().clear();
    assert!(installed_with(&commands, home.path(), "test.appex", MODULE).is_err());
    *commands.0.paths.borrow_mut() = vec![extension.clone(), extension.clone()];
    assert!(installed_with(&commands, home.path(), "test.appex", MODULE).is_err());
    *commands.0.paths.borrow_mut() = vec![extension];
    *commands.0.activation.fail.borrow_mut() = Some("/usr/bin/codesign".into());
    assert!(installed_with(&commands, home.path(), "test.appex", MODULE).is_err());
    let state = home.path().join(".local/share/fskit-native").join(MODULE);
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("activation-pending"), b"pending").unwrap();
    assert!(installed_with(&commands, home.path(), "test.appex", MODULE).is_err());
    assert!(commands.0.mutations.borrow().is_empty());
}

#[test]
fn installed_discovery_leaves_approval_to_macos_without_reading_settings() {
    let home = tempfile::tempdir().unwrap();
    let commands = ReadOnlyCommands(registration(""));
    let extension = PathBuf::from("/shared/Installed.app/Contents/Extensions/test.appex");
    *commands.0.paths.borrow_mut() = vec![extension.clone()];
    // Neither missing nor unreadable settings prevent discovery. A directory at
    // the plist path forces any attempted read to fail, even when tests run as root.
    for unreadable in [false, true] {
        if unreadable {
            fs::create_dir_all(settings(home.path())).unwrap();
        }
        assert_eq!(
            installed_with(&commands, home.path(), "test.appex", MODULE).unwrap(),
            extension
        );
    }
    assert!(commands.0.mutations.borrow().is_empty());
}

#[test]
fn shared_installation_locks_allow_mounts_and_exclude_setup() {
    let home = tempfile::tempdir().unwrap();
    let first = setup_lock(home.path()).unwrap();
    let second = setup_lock(home.path()).unwrap();
    let setup = setup_lock(home.path()).unwrap();
    first.try_lock_shared().unwrap();
    second.try_lock_shared().unwrap();
    assert!(setup.try_lock().is_err());
    drop(first);
    assert!(setup.try_lock().is_err());
    drop(second);
    setup.try_lock().unwrap();
    let mount = setup_lock(home.path()).unwrap();
    assert!(mount.try_lock_shared().is_err());
    drop(setup);
    mount.try_lock_shared().unwrap();
}

#[derive(Default)]
struct FakeCommands {
    calls: RefCell<Vec<String>>,
    mounts: String,
    fail: RefCell<Option<String>>,
}

impl Commands for FakeCommands {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
        self.calls.borrow_mut().push(program.to_owned());
        if self.fail.borrow().as_deref() == Some(program) {
            self.fail.borrow_mut().take();
            return Ok(Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: vec![],
                stderr: b"injected failure".to_vec(),
            });
        }
        let stdout = match program {
            "/usr/bin/plutil" => fs::read(Path::new(args.last().unwrap()))?,
            "/sbin/mount" => self.mounts.as_bytes().to_vec(),
            "/usr/libexec/PlistBuddy" => {
                let path = Path::new(args[2]);
                let contents = fs::read(path)?;
                // Model PlistBuddy's rejection of an existing empty file.
                assert!(
                    !contents.is_empty(),
                    "PlistBuddy cannot parse an empty file"
                );
                let mut modules: Vec<String> = if contents.starts_with(b"<?xml") {
                    assert!(String::from_utf8_lossy(&contents).contains("<array/>"));
                    Vec::new()
                } else {
                    serde_json::from_slice(&contents)?
                };
                assert!(args[1].to_string_lossy().starts_with("Add : string "));
                modules.push(
                    args[1]
                        .to_string_lossy()
                        .strip_prefix("Add : string ")
                        .unwrap()
                        .to_owned(),
                );
                fs::write(path, serde_json::to_vec(&modules)?)?;
                vec![]
            }
            "/usr/bin/pgrep" => b"1234\n".to_vec(),
            "/usr/bin/curl" => {
                fs::write(Path::new(args.last().unwrap()), b"wrong download")?;
                vec![]
            }
            _ => vec![],
        };
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout,
            stderr: vec![],
        })
    }
}

fn settings(home: &Path) -> PathBuf {
    home.join("Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist")
}

fn seed_settings(home: &Path, modules: &[&str]) {
    let settings = settings(home);
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(settings, serde_json::to_vec(modules).unwrap()).unwrap();
}

#[test]
fn enablement_preserves_other_modules_and_does_not_restart_twice() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &["com.example.other"]);
    let commands = FakeCommands::default();
    enable_module(&commands, home.path(), home.path()).unwrap();
    let modules: Vec<String> =
        serde_json::from_slice(&fs::read(settings(home.path())).unwrap()).unwrap();
    assert_eq!(modules, ["com.example.other", MODULE]);
    assert!(!home.path().join("activation-pending").exists());
    let calls = commands.calls.borrow().len();
    enable_module(&commands, home.path(), home.path()).unwrap();
    assert_eq!(commands.calls.borrow()[calls..], ["/usr/bin/plutil"]);
    let parent = settings(home.path()).parent().unwrap().to_path_buf();
    let backup = fs::read_dir(parent)
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("enabledModules.backup-")
        })
        .unwrap();
    let original: Vec<String> = serde_json::from_slice(&fs::read(backup.path()).unwrap()).unwrap();
    assert_eq!(original, ["com.example.other"]);
}

#[test]
fn fresh_enablement_creates_the_settings_array() {
    let home = tempfile::tempdir().unwrap();
    enable_module(&FakeCommands::default(), home.path(), home.path()).unwrap();
    let modules: Vec<String> =
        serde_json::from_slice(&fs::read(settings(home.path())).unwrap()).unwrap();
    assert_eq!(modules, [MODULE]);
}

#[cfg(target_os = "macos")]
#[test]
fn fresh_enablement_uses_a_valid_plist_with_system_tools() {
    struct PlistCommands(FakeCommands);
    impl Commands for PlistCommands {
        fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
            match program {
                "/usr/bin/plutil" | "/usr/libexec/PlistBuddy" => {
                    SystemCommands.output(program, args)
                }
                _ => self.0.output(program, args),
            }
        }
    }
    // Only plist tools run for real, with a temporary home. Never restart the
    // user's agent or touch their FSKit settings in a unit test.
    let home = tempfile::tempdir().unwrap();
    let commands = PlistCommands(FakeCommands::default());
    enable_module(&commands, home.path(), home.path()).unwrap();
    let json = commands
        .checked(
            "/usr/bin/plutil",
            &[
                "-convert".as_ref(),
                "json".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                settings(home.path()).as_os_str(),
            ],
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&json).unwrap(),
        [MODULE]
    );
    enable_module(&commands, home.path(), home.path()).unwrap();
}

#[test]
fn activation_read_errors_preserve_kind_and_explain_privacy_denials() {
    let path = Path::new("/private/settings/enabledModules.plist");
    let error = activation_access_error(path, io::Error::from_raw_os_error(libc::EPERM));
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("tailscaled"));
    assert!(
        error
            .to_string()
            .contains("/private/settings/enabledModules.plist")
    );
    let original = io::Error::from_raw_os_error(libc::EIO);
    let kind = original.kind();
    let error = activation_access_error(path, original);
    assert_eq!(error.kind(), kind);
    assert!(!error.to_string().contains("privacy controls"));
}

#[test]
fn unreadable_settings_stop_before_commands_or_activation_changes() {
    let home = tempfile::tempdir().unwrap();
    // A directory produces a real read failure even when tests run as root.
    fs::create_dir_all(settings(home.path())).unwrap();
    let commands = FakeCommands::default();
    let error = enable_module(&commands, home.path(), home.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot read FSKit activation settings")
    );
    assert!(commands.calls.borrow().is_empty());
    assert!(settings(home.path()).is_dir());
    assert!(!home.path().join("activation-pending").exists());
}

#[test]
fn active_mounts_block_settings_changes_and_agent_restart() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &["com.example.other"]);
    let before = fs::read(settings(home.path())).unwrap();
    let commands = FakeCommands {
        mounts: "volume on /tmp/a (local, fskit, mounted by user)\n".into(),
        ..Default::default()
    };
    assert!(enable_module(&commands, home.path(), home.path()).is_err());
    assert_eq!(before, fs::read(settings(home.path())).unwrap());
    assert!(
        !commands
            .calls
            .borrow()
            .iter()
            .any(|call| call == "/bin/kill")
    );
    assert!(!has_fskit_mount("volume on /tmp/fskit (local, apfs)\n"));
    // An already-enabled module needs no restart, even with active volumes.
    seed_settings(home.path(), &[MODULE]);
    enable_module(&commands, home.path(), home.path()).unwrap();
}

#[test]
fn failed_activation_retries_without_duplicate_registration() {
    let home = tempfile::tempdir().unwrap();
    let commands = FakeCommands::default();
    *commands.fail.borrow_mut() = Some("/bin/kill".into());
    assert!(enable_module(&commands, home.path(), home.path()).is_err());
    assert!(home.path().join("activation-pending").exists());
    enable_module(&commands, home.path(), home.path()).unwrap();
    let modules: Vec<String> =
        serde_json::from_slice(&fs::read(settings(home.path())).unwrap()).unwrap();
    assert_eq!(modules, [MODULE]);
    assert!(!home.path().join("activation-pending").exists());
}

#[test]
fn malformed_settings_fail_without_replacing_them() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[]);
    fs::write(settings(home.path()), b"not valid settings").unwrap();
    let commands = FakeCommands::default();
    assert!(enable_module(&commands, home.path(), home.path()).is_err());
    assert_eq!(
        fs::read(settings(home.path())).unwrap(),
        b"not valid settings"
    );
    assert_eq!(*commands.calls.borrow(), ["/usr/bin/plutil"]);
}

fn enable_module(commands: &impl Commands, home: &Path, base: &Path) -> io::Result<()> {
    enable_module_id(commands, home, base, MODULE)
}

struct RegistrationCommands {
    activation: FakeCommands,
    paths: RefCell<Vec<PathBuf>>,
    mutations: RefCell<Vec<String>>,
}
impl Commands for RegistrationCommands {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
        let text = if program == "/usr/bin/plutil" && args[0] == "-extract" {
            MODULE.to_owned()
        } else if program == "/usr/bin/pluginkit" {
            match args[0].to_str().unwrap() {
                "-m" => self
                    .paths
                    .borrow()
                    .iter()
                    .map(|p| format!("Path = {}\n", p.display()))
                    .collect(),
                "-r" => {
                    self.mutations.borrow_mut().push("unregister".into());
                    self.paths.borrow_mut().retain(|p| p != Path::new(args[1]));
                    String::new()
                }
                "-a" => {
                    self.mutations.borrow_mut().push("register".into());
                    let path = PathBuf::from(args[1]);
                    if !self.paths.borrow().contains(&path) {
                        self.paths.borrow_mut().push(path);
                    }
                    String::new()
                }
                _ => panic!("unexpected pluginkit command"),
            }
        } else {
            return self.activation.output(program, args);
        };
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: text.into_bytes(),
            stderr: vec![],
        })
    }
}
fn registration(mounts: &str) -> RegistrationCommands {
    RegistrationCommands {
        activation: FakeCommands {
            mounts: mounts.into(),
            ..Default::default()
        },
        paths: RefCell::new(vec![PathBuf::from(
            "/old/Example.app/Contents/Extensions/Example.appex",
        )]),
        mutations: RefCell::default(),
    }
}
fn upgrade(commands: &RegistrationCommands, home: &Path) -> io::Result<()> {
    register_with(
        commands,
        home,
        home,
        Path::new("/new/Example.app"),
        "Example.appex",
        MODULE,
    )
}

#[test]
fn upgrade_restarts_an_enabled_module_and_reuses_matching_registration() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE, "org.example.other"]);
    let commands = registration("");
    upgrade(&commands, home.path()).unwrap();
    assert_eq!(*commands.mutations.borrow(), ["unregister", "register"]);
    assert!(
        commands
            .activation
            .calls
            .borrow()
            .iter()
            .any(|c| c == "/bin/kill")
    );
    let calls = commands.activation.calls.borrow().len();
    upgrade(&commands, home.path()).unwrap();
    assert!(
        !commands.activation.calls.borrow()[calls..]
            .iter()
            .any(|c| c == "/bin/kill")
    );
    assert_eq!(*commands.mutations.borrow(), ["unregister", "register"]);
}

#[test]
fn upgrade_with_mounted_volume_does_not_change_registration() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE]);
    let commands = registration("resource on /mounted (local, fskit)\n");
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(commands.mutations.borrow().is_empty());
    assert!(!home.path().join("activation-pending").exists());
}

#[test]
fn failed_upgrade_restart_is_retried_without_unregistering_again() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE]);
    let commands = registration("");
    *commands.activation.fail.borrow_mut() = Some("/bin/kill".into());
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(home.path().join("activation-pending").exists());
    upgrade(&commands, home.path()).unwrap();
    assert_eq!(*commands.mutations.borrow(), ["unregister", "register"]);
    assert!(!home.path().join("activation-pending").exists());
}

#[test]
fn upgrade_checks_settings_before_unregistering() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(settings(home.path())).unwrap();
    let commands = registration("");
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(commands.mutations.borrow().is_empty());
}

struct PrivateSettings(RegistrationCommands);

impl Commands for PrivateSettings {
    fn output(&self, program: &str, args: &[&OsStr]) -> io::Result<Output> {
        assert_ne!(program, "/usr/libexec/PlistBuddy");
        self.0.output(program, args)
    }

    fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
}

fn manual_registration(commands: &PrivateSettings, home: &Path, base: &Path) -> io::Result<()> {
    register_with(
        commands,
        home,
        base,
        Path::new("/new/Example.app"),
        "Example.appex",
        MODULE,
    )
}

#[test]
fn private_settings_allow_registration_and_leave_approval_to_mounting() {
    for fresh in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join(".local/share/fskit-native").join(MODULE);
        fs::create_dir_all(&base).unwrap();
        let commands = PrivateSettings(registration(""));
        if fresh {
            commands.0.paths.borrow_mut().clear();
        }
        manual_registration(&commands, home.path(), &base).unwrap();
        assert!(!base.join("activation-pending").exists());
        assert!(!settings(home.path()).exists());
        assert_eq!(
            installed_with(&commands, home.path(), "Example.appex", MODULE).unwrap(),
            Path::new("/new/Example.app/Contents/Extensions/Example.appex")
        );
        let mutations = commands.0.mutations.borrow().clone();
        let calls = commands.0.activation.calls.borrow().len();
        manual_registration(&commands, home.path(), &base).unwrap();
        assert_eq!(*commands.0.mutations.borrow(), mutations);
        assert!(
            !commands.0.activation.calls.borrow()[calls..]
                .iter()
                .any(|c| c == "/bin/kill")
        );
    }
}

#[test]
fn manual_approval_preserves_idle_and_restart_guards() {
    let home = tempfile::tempdir().unwrap();
    let base = home.path().join(".local/share/fskit-native").join(MODULE);
    fs::create_dir_all(&base).unwrap();
    let busy = PrivateSettings(registration("resource on /mounted (local, fskit)\n"));
    assert!(manual_registration(&busy, home.path(), &base).is_err());
    assert!(busy.0.mutations.borrow().is_empty());

    let commands = PrivateSettings(registration(""));
    *commands.0.activation.fail.borrow_mut() = Some("/bin/kill".into());
    assert!(manual_registration(&commands, home.path(), &base).is_err());
    assert!(base.join("activation-pending").exists());
    assert!(installed_with(&commands, home.path(), "Example.appex", MODULE).is_err());
    manual_registration(&commands, home.path(), &base).unwrap();
    assert!(!base.join("activation-pending").exists());
    assert_eq!(*commands.0.mutations.borrow(), ["unregister", "register"]);
}

#[test]
fn activation_accepts_different_module_identifiers() {
    let home = tempfile::tempdir().unwrap();
    let commands = FakeCommands::default();
    enable_module_id(&commands, home.path(), home.path(), "org.example.benchmark").unwrap();
    let modules: Vec<String> =
        serde_json::from_slice(&fs::read(settings(home.path())).unwrap()).unwrap();
    assert_eq!(modules, ["org.example.benchmark"]);
    assert!(!valid_identifier("../escape"));
    assert!(!valid_identifier(".."));
}

#[test]
fn failed_registration_can_retry_after_the_old_bundle_was_removed() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE]);
    let commands = registration("");
    *commands.activation.fail.borrow_mut() = Some(LSREGISTER.into());
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(commands.paths.borrow().is_empty());
    assert!(home.path().join("activation-pending").exists());
    upgrade(&commands, home.path()).unwrap();
    assert_eq!(*commands.mutations.borrow(), ["unregister", "register"]);
    assert!(!home.path().join("activation-pending").exists());
}

#[test]
fn invalid_signature_or_registration_path_cannot_remove_an_existing_bundle() {
    let home = tempfile::tempdir().unwrap();
    seed_settings(home.path(), &[MODULE]);
    let commands = registration("");
    *commands.activation.fail.borrow_mut() = Some("/usr/bin/codesign".into());
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(commands.mutations.borrow().is_empty());
    *commands.paths.borrow_mut() = vec![PathBuf::from("/unexpected/Example.appex")];
    assert!(upgrade(&commands, home.path()).is_err());
    assert!(commands.mutations.borrow().is_empty());
}
