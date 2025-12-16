//! CI provider trait and implementations.
//!
//! This module provides a trait-based abstraction for CI/CD providers.
//!
//! # Provider Implementations
//!
//! - [`LocalProvider`]: Fallback provider for local development (included in this crate)
//! - `cuenv-github`: [`GitHubCIProvider`](https://docs.rs/cuenv-github) for GitHub Actions
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_ci::provider::{CIProvider, local::LocalProvider};
//!
//! // Try to detect a CI provider, fall back to local
//! let provider = LocalProvider::detect().expect("LocalProvider always detects");
//! let files = provider.changed_files().await?;
//! ```

use crate::context::CIContext;
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::path::PathBuf;

/// Trait for CI/CD provider integrations.
///
/// Implementations provide platform-specific functionality for:
/// - Detecting the CI environment
/// - Finding changed files
/// - Creating and updating check runs/statuses
/// - Uploading reports
#[async_trait]
pub trait CIProvider: Send + Sync {
    /// Detect if running in this CI environment.
    ///
    /// Returns `Some(Self)` if the current environment matches this provider,
    /// `None` otherwise.
    fn detect() -> Option<Self>
    where
        Self: Sized;

    /// Get normalized CI context.
    fn context(&self) -> &CIContext;

    /// Get files changed in this build.
    ///
    /// For PRs, this returns files changed relative to the base branch.
    /// For pushes, this returns files changed in the pushed commits.
    async fn changed_files(&self) -> Result<Vec<PathBuf>>;

    /// Create a check/status for a project pipeline.
    async fn create_check(&self, name: &str) -> Result<CheckHandle>;

    /// Update check with progress summary.
    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()>;

    /// Complete check with final report (renders to provider-specific format).
    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()>;

    /// Upload report artifact, return URL if available.
    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>>;
}

pub mod local;

pub use local::LocalProvider;
