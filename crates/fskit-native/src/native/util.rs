use objc2::rc::Retained;
use objc2_foundation::{NSError, NSPOSIXErrorDomain};
use std::ffi::c_int;

pub(crate) fn posix_err(code: c_int) -> *mut NSError {
    Retained::autorelease_ptr(NSError::new(code as _, unsafe { NSPOSIXErrorDomain }))
}

pub(crate) fn io_err(error: &std::io::Error) -> *mut NSError {
    use std::io::ErrorKind;
    posix_err(error.raw_os_error().unwrap_or(match error.kind() {
        ErrorKind::NotFound => libc::ENOENT,
        ErrorKind::PermissionDenied => libc::EACCES,
        ErrorKind::InvalidInput | ErrorKind::InvalidData => libc::EINVAL,
        ErrorKind::NotADirectory => libc::ENOTDIR,
        ErrorKind::IsADirectory => libc::EISDIR,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::WouldBlock => libc::EBUSY,
        ErrorKind::NotConnected => libc::ENXIO,
        ErrorKind::ReadOnlyFilesystem => libc::EROFS,
        ErrorKind::Unsupported => libc::ENOTSUP,
        _ => libc::EIO,
    }))
}
