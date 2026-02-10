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
use cuenv_core::DryRun;
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
    pub dry_run: DryRun,
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
            dry_run: DryRun::No,
            download_base_url: None,
        }
    }

    /// Sets the dry-run flag.
    #[must_use]
    pub const fn with_dry_run(mut self, dry_run: DryRun) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_context_new() {
        let ctx = BackendContext::new("my-app", "1.0.0");
        assert_eq!(ctx.name, "my-app");
        assert_eq!(ctx.version, "1.0.0");
        assert!(!ctx.dry_run.is_dry_run());
        assert!(ctx.download_base_url.is_none());
    }

    #[test]
    fn test_backend_context_with_dry_run() {
        let ctx = BackendContext::new("my-app", "1.0.0").with_dry_run(DryRun::Yes);
        assert!(ctx.dry_run.is_dry_run());
    }

    #[test]
    fn test_backend_context_with_download_url() {
        let ctx =
            BackendContext::new("my-app", "1.0.0").with_download_url("https://github.com/releases");
        assert_eq!(
            ctx.download_base_url,
            Some("https://github.com/releases".to_string())
        );
    }

    #[test]
    fn test_backend_context_builder_chain() {
        let ctx = BackendContext::new("test", "2.0.0")
            .with_dry_run(DryRun::Yes)
            .with_download_url("https://example.com");

        assert_eq!(ctx.name, "test");
        assert_eq!(ctx.version, "2.0.0");
        assert!(ctx.dry_run.is_dry_run());
        assert_eq!(
            ctx.download_base_url,
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_publish_result_success() {
        let result = PublishResult::success("github", "Published successfully");
        assert!(result.success);
        assert_eq!(result.backend, "github");
        assert_eq!(result.message, "Published successfully");
        assert!(result.url.is_none());
    }

    #[test]
    fn test_publish_result_success_with_url() {
        let result = PublishResult::success_with_url(
            "github",
            "Released",
            "https://github.com/repo/releases/v1.0.0",
        );
        assert!(result.success);
        assert_eq!(
            result.url,
            Some("https://github.com/repo/releases/v1.0.0".to_string())
        );
    }

    #[test]
    fn test_publish_result_dry_run() {
        let result = PublishResult::dry_run("homebrew", "Would update formula");
        assert!(result.success);
        assert!(result.message.starts_with("[dry-run]"));
        assert!(result.message.contains("Would update formula"));
    }

    #[test]
    fn test_publish_result_failure() {
        let result = PublishResult::failure("crates-io", "Upload failed");
        assert!(!result.success);
        assert_eq!(result.backend, "crates-io");
        assert_eq!(result.message, "Upload failed");
        assert!(result.url.is_none());
    }

    #[test]
    fn test_backend_context_debug() {
        let ctx = BackendContext::new("app", "1.0");
        let debug_str = format!("{ctx:?}");
        assert!(debug_str.contains("BackendContext"));
        assert!(debug_str.contains("app"));
    }

    #[test]
    fn test_publish_result_debug() {
        let result = PublishResult::success("test", "ok");
        let debug_str = format!("{result:?}");
        assert!(debug_str.contains("PublishResult"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_backend_context_clone() {
        let ctx = BackendContext::new("app", "1.0")
            .with_dry_run(DryRun::Yes)
            .with_download_url("https://example.com");
        let cloned = ctx.clone();
        assert_eq!(ctx.name, cloned.name);
        assert_eq!(ctx.version, cloned.version);
        assert_eq!(ctx.dry_run, cloned.dry_run);
        assert_eq!(ctx.download_base_url, cloned.download_base_url);
    }

    #[test]
    fn test_publish_result_clone() {
        let result = PublishResult::success_with_url("github", "Released", "https://url");
        let cloned = result.clone();
        assert_eq!(result.backend, cloned.backend);
        assert_eq!(result.success, cloned.success);
        assert_eq!(result.url, cloned.url);
        assert_eq!(result.message, cloned.message);
    }
}
