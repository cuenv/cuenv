use crate::context::CIContext;
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::path::PathBuf;

#[async_trait]
pub trait CIProvider: Send + Sync {
    /// Detect if running in this CI environment
    fn detect() -> Option<Self>
    where
        Self: Sized;

    /// Get normalized CI context
    fn context(&self) -> &CIContext;

    /// Get files changed in this build
    async fn changed_files(&self) -> Result<Vec<PathBuf>>;

    /// Create a check/status for a project pipeline
    async fn create_check(&self, name: &str) -> Result<CheckHandle>;

    /// Update check with progress summary
    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()>;

    /// Complete check with final report (renders to provider-specific format)
    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()>;

    /// Upload report artifact, return URL if available
    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>>;
}

pub mod github;
pub mod local;
