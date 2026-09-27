//! Read-only filesystem views over Casita repositories.
//!
//! [`FilesystemView`] resolves named roots, traverses directories, and serves
//! file ranges lazily without materializing trees. The `linux-fuse` feature
//! provides Linux FUSE through `fuse-backend-rs`; `darwin-fskit` provides native Rust
//! FSKit mounts on macOS. Both platform transports are enabled by default.
//! Applications choose roots and enforce access policy, while this crate owns
//! traversal and transport mechanics.

mod view;

pub use view::{
    node_kind, ContentKey, ContentReader, ContentStream, FilesystemEntry, FilesystemNode,
    FilesystemNodeKind, FilesystemView,
};

#[cfg(all(target_os = "linux", feature = "linux-fuse"))]
pub mod fuse;

#[cfg(all(unix, feature = "darwin-fskit"))]
pub mod darwin;

#[cfg(all(test, target_os = "linux", feature = "linux-fuse"))]
mod test_support;
