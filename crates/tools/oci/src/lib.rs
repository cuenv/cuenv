//! OCI-based binary provider for cuenv.
//!
//! This crate provides functionality to:
//! - Resolve OCI image references to content-addressed digests
//! - Extract binaries from OCI container images
//! - Cache binaries by digest for hermetic builds
//!
//! # Example
//!
//! ```
//! use cuenv_tools_oci::{OciCache, Platform};
//!
//! let temp = tempfile::tempdir()?;
//! let cache = OciCache::new(temp.path().join("cache"));
//! let source = temp.path().join("tool");
//! std::fs::write(&source, b"example binary")?;
//!
//! // Cache an extracted binary under its content digest.
//! let digest = "sha256:f401fc3ba33b996c7478b5b9a67f412970f7ebb87ab1a9a8ea4c7c027b50a5b2";
//! let path = cache.store_binary(digest, "tool", &source)?;
//! assert_eq!(cache.binary(digest, "tool"), Some(path));
//!
//! let platform = Platform::new("linux", "x86_64");
//! assert_eq!(platform.to_oci_platform(), "linux/amd64");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![warn(missing_docs)]

mod cache;
mod error;
mod extract;
mod platform;
mod provider;
mod registry;

pub use cache::OciCache;
pub use error::{Error, Result};
pub use extract::extract_from_layers;
pub use platform::{Platform, current_platform, normalize_platform};
pub use provider::OciToolProvider;
pub use registry::OciClient;

/// Media type for OCI image layers (gzip compressed tar).
pub const OCI_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
