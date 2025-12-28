//! OCI-based binary provider for cuenv.
//!
//! This crate provides functionality to:
//! - Resolve OCI image references to content-addressed digests
//! - Extract binaries from OCI images (Homebrew bottles and container images)
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
//! let digest = client.resolve_digest("ghcr.io/homebrew/core/jq:1.7.1", "darwin-arm64").await?;
//!
//! // Fetch and extract binary
//! let binary_path = client.fetch_binary(&digest, &cache).await?;
//! ```

#![warn(missing_docs)]

mod cache;
mod error;
mod extract;
pub mod homebrew;
mod platform;
mod registry;

pub use cache::OciCache;
pub use error::{Error, Result};
pub use extract::{
    extract_from_layers, extract_homebrew_binary, extract_homebrew_bottle, relocate_homebrew_bottle,
};
pub use homebrew::{
    HomebrewFormula, fetch_formula, formula_name_from_image, resolve_with_deps,
    to_homebrew_platform,
};
pub use platform::{Platform, current_platform, normalize_platform};
pub use registry::OciClient;

/// Media type for Homebrew bottles (gzip compressed tar).
pub const HOMEBREW_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// Check if an image reference is a Homebrew bottle.
///
/// Homebrew bottles are hosted at `ghcr.io/homebrew/*`.
#[must_use]
pub fn is_homebrew_image(image: &str) -> bool {
    image.starts_with("ghcr.io/homebrew/")
}
