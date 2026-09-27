//! Native Objective-C classes and resource ownership for an FSKit extension.
//!
//! Call [`register`] from the extension's constructor, before Apple's extension
//! entry point. Set `EXExtensionPrincipalClass` to [`PRINCIPAL_CLASS`] in the
//! bundle's Info.plist. The application owns its bundle identity and signing.

mod filesystem;
mod item;
mod util;
mod volume;

use crate::Filesystem;
use objc2::ClassType;
use std::{io, path::Path, sync::OnceLock};

pub const PRINCIPAL_CLASS: &str = "FSKitNativeFileSystem";

pub enum Resource<'a> {
    Path(&'a Path),
    /// BSD device name. The crate does not open or mutate the device.
    BlockDevice(&'a str),
}

pub struct Extension {
    pub name: &'static str,
    /// Must match FSShortName in Info.plist.
    pub filesystem_type: &'static str,
    pub path_resources: bool,
    pub block_resources: bool,
    pub open: fn(Resource<'_>) -> io::Result<Box<dyn Filesystem>>,
}

static EXTENSION: OnceLock<Extension> = OnceLock::new();

/// Register one filesystem implementation per extension process.
pub fn register(extension: Extension) -> io::Result<()> {
    EXTENSION.set(extension).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "filesystem already registered",
        )
    })?;
    let _ = filesystem::FileSystem::class();
    Ok(())
}

fn configuration() -> &'static Extension {
    EXTENSION
        .get()
        .expect("register before native class initialization")
}
