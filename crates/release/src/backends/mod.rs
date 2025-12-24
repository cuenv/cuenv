//! Release distribution backends.
//!
//! This module defines the [`ReleaseBackend`] trait that provider crates
//! can implement to support release distribution.
//!
//! # Architecture
//!
//! The release crate provides:
//! - [`ReleaseBackend`] trait - interface for publishing artifacts
//! - [`BackendContext`] - common context passed to backends
//! - [`PublishResult`] - result type for publish operations
//!
//! Provider crates implement `ReleaseBackend`:
//! - `cuenv-github` - GitHub Releases
//! - `cuenv-homebrew` - Homebrew tap updates
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_release::backends::{ReleaseBackend, BackendContext, PublishResult};
//! use cuenv_release::artifact::PackagedArtifact;
//!
//! struct MyBackend;
//!
//! impl ReleaseBackend for MyBackend {
//!     fn name(&self) -> &'static str { "my-backend" }
//!
//!     fn publish<'a>(
//!         &'a self,
//!         ctx: &'a BackendContext,
//!         artifacts: &'a [PackagedArtifact],
//!     ) -> Pin<Box<dyn Future<Output = Result<PublishResult>> + Send + 'a>> {
//!         Box::pin(async move {
//!             // Upload artifacts...
//!             Ok(PublishResult::success("my-backend", "Published"))
//!         })
//!     }
//! }
//! ```

use crate::artifact::PackagedArtifact;
use crate::error::Result;
use std::future::Future;
use std::pin::Pin;

/// Configuration common to all backends.
#[derive(Debug, Clone)]
pub struct BackendContext {
    /// Project/binary name
    pub name: String,
    /// Version being released (without 'v' prefix)
    pub version: String,
    /// Whether this is a dry-run (no actual publishing)
    pub dry_run: bool,
    /// Base URL for downloading release assets (e.g., GitHub releases URL)
    pub download_base_url: Option<String>,
}

impl BackendContext {
    /// Creates a new backend context.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            dry_run: false,
            download_base_url: None,
        }
    }

    /// Sets the dry-run flag.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Sets the download base URL.
    #[must_use]
    pub fn with_download_url(mut self, url: impl Into<String>) -> Self {
        self.download_base_url = Some(url.into());
        self
    }
}

/// Result of a backend publish operation.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// Name of the backend
    pub backend: String,
    /// Whether publishing succeeded
    pub success: bool,
    /// URL or identifier of the published artifact (if any)
    pub url: Option<String>,
    /// Human-readable message
    pub message: String,
}

impl PublishResult {
    /// Creates a successful result.
    #[must_use]
    pub fn success(backend: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            success: true,
            url: None,
            message: message.into(),
        }
    }

    /// Creates a successful result with URL.
    #[must_use]
    pub fn success_with_url(
        backend: impl Into<String>,
        message: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            backend: backend.into(),
            success: true,
            url: Some(url.into()),
            message: message.into(),
        }
    }

    /// Creates a dry-run result.
    #[must_use]
    pub fn dry_run(backend: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            success: true,
            url: None,
            message: format!("[dry-run] {}", message.into()),
        }
    }

    /// Creates a failure result.
    #[must_use]
    pub fn failure(backend: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            success: false,
            url: None,
            message: message.into(),
        }
    }
}

/// Trait for release distribution backends.
///
/// Each backend handles publishing artifacts to a specific distribution channel
/// (GitHub Releases, Homebrew, crates.io, CUE registry, etc.).
///
/// # Implementors
///
/// - `cuenv-github` - GitHub Releases backend
/// - `cuenv-homebrew` - Homebrew tap backend
///
/// # Example
///
/// See module-level documentation for implementation example.
pub trait ReleaseBackend: Send + Sync {
    /// Returns the name of this backend (e.g., "github", "homebrew").
    fn name(&self) -> &'static str;

    /// Publishes the given artifacts to this backend.
    ///
    /// # Arguments
    /// * `ctx` - Common context (version, dry-run flag, etc.)
    /// * `artifacts` - Packaged artifacts to publish
    ///
    /// # Returns
    /// A [`PublishResult`] indicating success or failure.
    fn publish<'a>(
        &'a self,
        ctx: &'a BackendContext,
        artifacts: &'a [PackagedArtifact],
    ) -> Pin<Box<dyn Future<Output = Result<PublishResult>> + Send + 'a>>;
}
