//! Casita repository integration and benchmark fixtures for native FSKit.

pub use fskit_native as filesystem;
mod memory;
pub use memory::{fixture, Entry, MemoryFilesystem, FIRST_FILE_ID, ROOT_ID, SCRIPT, SIZES};

#[cfg(feature = "repository")]
pub mod repository;
#[cfg(feature = "repository")]
mod repository_adapter;
#[cfg(feature = "repository")]
pub use repository_adapter::PublishRoot;

#[cfg(test)]
mod contract_tests;
