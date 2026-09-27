//! Native Git object formats and repository services.
//!
//! Object encodings remain available in the portable profile. Repository
//! operations, Gix integration, fetch, and HTTP retain their own feature gates.

mod objects;
pub use objects::*;

#[cfg(feature = "git-fetch")]
pub(crate) mod fetch;
#[cfg(feature = "git")]
pub(crate) mod gix_odb;
#[cfg(feature = "git-http")]
pub(crate) mod http;
#[cfg(feature = "native")]
pub(crate) mod repository;
