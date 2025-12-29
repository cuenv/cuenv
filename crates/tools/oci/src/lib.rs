//! OCI-based binary provider for cuenv.
//!
//! This crate provides functionality to:
//! - Resolve OCI image references to content-addressed digests
//! - Extract binaries from OCI container images
//! - Cache binaries by digest for hermetic builds
//!
//! # Example
//!
//! ```ignore
//! use cuenv_tools_oci::{OciClient, OciCache};
//!
//! let client = OciClient::new()?;
//! let cache = OciCache::default();
//!
//! // Resolve image to platform-specific digest
//! let digest = client.resolve_digest("nginx:1.25-alpine", "linux-arm64").await?;
//!
//! // Fetch layers
//! let layers = client.pull_layers(&digest, &cache).await?;
//! ```

#![warn(missing_docs)]

mod cache;
mod error;
mod extract;
mod platform;
mod registry;

pub use cache::OciCache;
pub use error::{Error, Result};
pub use extract::extract_from_layers;
pub use platform::{Platform, current_platform, normalize_platform};
pub use registry::OciClient;

/// Media type for OCI image layers (gzip compressed tar).
pub const OCI_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
