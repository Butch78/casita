//! Real rootless production-native mount and independent-process lifecycle checks.
#![cfg(target_os = "macos")]

#[test]
#[ignore = "requires macOS 26+ and an installed, enabled Casita extension"]
fn native_mount_lifecycle() -> anyhow::Result<()> {
    lifecycle(None)
}

#[test]
#[ignore = "changes registration; requires signed bundles and exclusive FSKit access"]
fn native_setup_upgrade() -> anyhow::Result<()> {
    let bundle = std::env::var_os("CASITA_FSKIT_APP_BUNDLE")
        .ok_or_else(|| anyhow::anyhow!("set CASITA_FSKIT_APP_BUNDLE"))?;
    let upgrade = std::env::var_os("CASITA_FSKIT_UPGRADE_APP_BUNDLE")
        .ok_or_else(|| anyhow::anyhow!("set CASITA_FSKIT_UPGRADE_APP_BUNDLE"))?;
    casita_fs::darwin::setup::register_native(std::path::Path::new(&bundle))?;
    lifecycle(Some(std::path::Path::new(&upgrade)))
}

fn lifecycle(upgrade: Option<&std::path::Path>) -> anyhow::Result<()> {
    use casita_fs::{darwin::PersistentMount, ContentReader};
    use std::{fs, os::unix::fs::PermissionsExt};
    let bundle = casita_fs::darwin::setup::installed_native()?
        .app()
        .to_owned();
    let work = tempfile::tempdir()?;
    let source = work.path().join("source");
    fs::create_dir(&source)?;
    fs::write(source.join("data"), b"native production bytes")?;
    fs::write(
        source.join("run"),
        b"#!/bin/sh\nprintf native-production-ok",
    )?;
    fs::set_permissions(source.join("run"), fs::Permissions::from_mode(0o555))?;
    let repo_path = work.path().join("repository");
    let runtime = tokio::runtime::Runtime::new()?;
    let repository = runtime.block_on(casita::Repository::local(&repo_path))?;
    let (root, directory) = runtime.block_on(async {
        let key = repository
            .import(casita::import::FilesystemImport::new(
                &source,
                casita::RootName::try_from("fixture")?,
            ))
            .await?;
        let digest = casita::DirectoryId::new(key.native_digest().unwrap());
        let directory = repository.directory(&digest).await?.unwrap();
        repository.flush().await?;
        anyhow::Ok((
            casita::Node::Directory {
                digest,
                size: directory.size(),
            },
            directory,
        ))
    })?;
    let mut mount = PersistentMount::new(&repo_path, work.path())?;
    verify_extension(mount.path(), std::path::Path::new(&bundle))?;
    anyhow::ensure!(PersistentMount::new(&repo_path, work.path()).is_err());
    anyhow::ensure!(!mount.path().join("views/fixture").exists());
    let tree = mount.publish_root(b"fixture", root.clone())?;
    anyhow::ensure!(fs::read(tree.join("data"))? == b"native production bytes");
    let output = std::process::Command::new(tree.join("run")).output()?;
    anyhow::ensure!(output.status.success() && output.stdout == b"native-production-ok");
    let file_node = directory.get(b"data").unwrap().clone();
    let file = mount.publish_root(b"file", file_node.clone())?;
    anyhow::ensure!(fs::read(&file)? == b"native production bytes");
    let link = mount.publish_root(
        b"link",
        casita::Node::Symlink {
            target: "file".try_into()?,
        },
    )?;
    anyhow::ensure!(fs::read(link)? == b"native production bytes");
    anyhow::ensure!(mount.publish_root(b"fixture", root.clone())? == tree);
    let byte_file = mount.publish_root(b"file-\xff", file_node.clone())?;
    anyhow::ensure!(fs::read(byte_file)? == b"native production bytes");
    anyhow::ensure!(mount.publish_root(b"fixture", file_node).is_err());
    anyhow::ensure!(fs::write(&file, b"mutation").is_err());
    if let Some(upgrade) = upgrade {
        anyhow::ensure!(
            casita_fs::darwin::setup::register_native(upgrade).is_err(),
            "bundle upgrade unexpectedly allowed with an active mount"
        );
        anyhow::ensure!(fs::read(&file)? == b"native production bytes");
        anyhow::ensure!(
            casita_fs::darwin::setup::installed_native()?.app() == bundle,
            "refused upgrade changed the selected bundle"
        );
    }
    let held = fs::File::open(&file)?;
    anyhow::ensure!(mount.close().is_err(), "busy mount unexpectedly detached");
    drop(held);
    mount.close()?;
    anyhow::ensure!(!repo_path
        .join(casita_fs::darwin::native_protocol::ACTIVE)
        .exists());
    if let Some(upgrade) = upgrade {
        casita_fs::darwin::setup::register_native(upgrade)?;
    }
    let next_bundle = casita_fs::darwin::setup::installed_native()?
        .app()
        .to_owned();
    let mut reopened = PersistentMount::new(&repo_path, work.path())?;
    verify_extension(reopened.path(), &next_bundle)?;
    let tree = reopened.publish_root(b"fixture", root)?;
    anyhow::ensure!(fs::read(tree.join("data"))? == b"native production bytes");
    reopened.close()?;
    runtime.block_on(repository.flush())?;
    println!("native production mount, execution, typed publication, byte names, immutability, busy-close, cleanup and remount passed");
    Ok(())
}

fn verify_extension(mount: &std::path::Path, bundle: &std::path::Path) -> anyhow::Result<()> {
    use std::fs;
    let stats: serde_json::Value =
        serde_json::from_slice(&fs::read(mount.join("__casita_stats-0"))?)?;
    let pid = stats["pid"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("extension statistics omitted the process ID"))?;
    let process = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()?;
    anyhow::ensure!(
        process.status.success(),
        "cannot identify extension process"
    );
    let executable = String::from_utf8(process.stdout)?;
    anyhow::ensure!(
        fs::canonicalize(executable.trim())?
            == fs::canonicalize(bundle.join(
                "Contents/Extensions/casita-native-fskit-extension.appex/Contents/MacOS/casita-native-fskit-extension"
            ))?,
        "mount is served by a different extension bundle"
    );
    Ok(())
}

#[path = "native_mount/concurrent.rs"]
mod concurrent;
