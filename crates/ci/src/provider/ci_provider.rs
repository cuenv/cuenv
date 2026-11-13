//! Provider interface for CI pipeline reporting.

use super::ChangedFiles;
use crate::context::CIContext;
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;

/// Trait for CI/CD provider integrations.
///
/// Implementations provide platform-specific functionality for:
/// - Detecting the CI environment
/// - Comparing a run with its base, when it has one
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

    /// Compare this run with its base.
    ///
    /// For a pull request the base is the branch it targets; for a push it
    /// is the commit the branch was at before. A run with nothing to compare
    /// with reports [`ChangedFiles::NoBase`] and why. A base is never
    /// guessed: the parent of `HEAD` is not what a scheduled run, a tag or a
    /// push of several commits came after.
    async fn changed_files(&self) -> Result<ChangedFiles>;

    /// Create a check/status for a project pipeline.
    async fn create_check(&self, name: &str) -> Result<CheckHandle>;

    /// Update check with progress summary.
    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()>;

    /// Complete check with final report (renders to provider-specific format).
    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()>;

    /// Upload report artifact, return URL if available.
    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>>;
}
