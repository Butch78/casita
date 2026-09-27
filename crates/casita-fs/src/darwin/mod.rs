//! Persistent FSKit transport over Casita. Published file contents are immutable;
//! the controller publishes prepared directories through the VFS for coherent
//! directory and negative caches. FSKit does not supply build caller identity;
//! the caller must enforce build isolation (Seatbelt on macOS).
/// Native FSKit registration and activation.
pub mod setup;

#[cfg(target_os = "macos")]
mod native_mount;
#[doc(hidden)]
pub mod native_protocol;
#[cfg(target_os = "macos")]
pub use native_mount::NativeRepositoryMount;

/// Primary macOS mount: repository-backed native FSKit.
#[cfg(target_os = "macos")]
pub use native_mount::NativeRepositoryMount as PersistentMount;
