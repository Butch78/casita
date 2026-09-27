//! Rootless registration of the production Casita FSKit extension.
use std::{io, path::Path};

const EXTENSION: &str = "casita-native-fskit-extension.appex";
const MODULE: &str = "org.casita.fskit.extension";
const PROTOCOL_KEY: &str = "CasitaMountProtocol";

/// Find the shared installation without changing the user's registration.
pub fn installed_native() -> io::Result<fskit_native::setup::Installation> {
    let installed = fskit_native::setup::installed(EXTENSION, MODULE).map_err(setup_error)?;
    check_protocol(&installed.property(PROTOCOL_KEY).map_err(setup_error)?)?;
    Ok(installed)
}

fn setup_error(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!(
        "{error}; install a compatible bundle with `casita-fs-setup APP_BUNDLE` while FSKit volumes are idle"
    ))
}

fn check_protocol(value: &[u8]) -> io::Result<()> {
    if value.trim_ascii() != super::native_protocol::VERSION.trim_ascii() {
        return Err(setup_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "incompatible Casita FSKit mount protocol",
        )));
    }
    Ok(())
}

/// Verify and register the production bundle, upgrading an idle installation.
/// Activate automatically where permitted; macOS checks approval when mounting.
pub fn register_native(app: &Path) -> io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FSKit setup requires macOS",
        ));
    }
    let output = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", PROTOCOL_KEY, "raw"])
        .arg(
            app.join("Contents/Extensions")
                .join(EXTENSION)
                .join("Contents/Info.plist"),
        )
        .output()?;
    if !output.status.success() {
        return Err(setup_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle does not declare a Casita FSKit mount protocol",
        )));
    }
    check_protocol(&output.stdout)?;
    fskit_native::setup::register(app, EXTENSION, MODULE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_requires_an_explicit_compatible_version() {
        assert!(check_protocol(super::super::native_protocol::VERSION).is_ok());
        for value in [
            b"".as_slice(),
            b"casita-native-mount-v0",
            b"casita-native-mount-v2",
        ] {
            assert_eq!(
                check_protocol(value).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        let plist = include_str!("../../../casita-fskit/extension/Info.plist");
        assert!(plist.contains(
            std::str::from_utf8(super::super::native_protocol::VERSION.trim_ascii()).unwrap()
        ));
    }
}
