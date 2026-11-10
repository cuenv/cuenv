use super::CIProvider;
use super::git::{Checkout, Since};
use crate::context::CIContext;
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::collections::HashSet;
use std::path::PathBuf;

/// CI provider for local runs, optionally comparing against a Git base ref.
pub struct LocalProvider {
    context: CIContext,
    base_ref: Option<String>,
}

impl LocalProvider {
    /// Create a `LocalProvider` that compares against a specific base reference.
    /// This will detect changes between the base ref and HEAD, plus uncommitted changes.
    #[must_use]
    pub fn with_base_ref(base_ref: String) -> Self {
        Self {
            context: CIContext {
                provider: "local".to_string(),
                event: "manual".to_string(),
                ref_name: "current".to_string(),
                base_ref: Some(base_ref.clone()),
                sha: "current".to_string(),
            },
            base_ref: Some(base_ref),
        }
    }
}

#[async_trait]
impl CIProvider for LocalProvider {
    fn detect() -> Option<Self> {
        // Always available as fallback
        Some(Self {
            context: CIContext {
                provider: "local".to_string(),
                event: "manual".to_string(),
                ref_name: "current".to_string(),
                base_ref: None,
                sha: "current".to_string(),
            },
            base_ref: None,
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        let Some(checkout) = Checkout::discover() else {
            return Ok(Vec::new());
        };

        // Committed changes since the base ref: what is in HEAD that the
        // base ref does not have.
        let committed = self
            .base_ref
            .as_deref()
            .and_then(|base_ref| checkout.changed(Since::MergeBaseWith(base_ref)));

        // Always include uncommitted changes (staged + unstaged).
        let uncommitted = checkout.uncommitted().map_err(|error| {
            cuenv_core::Error::execution(format!(
                "Could not list the uncommitted changes of this checkout: {error}"
            ))
        })?;
        let changed: HashSet<PathBuf> =
            committed.into_iter().flatten().chain(uncommitted).collect();

        Ok(changed.into_iter().collect())
    }

    async fn create_check(&self, _name: &str) -> Result<CheckHandle> {
        Ok(CheckHandle {
            id: "local".to_string(),
        })
    }

    async fn update_check(&self, _handle: &CheckHandle, _summary: &str) -> Result<()> {
        Ok(())
    }

    async fn complete_check(&self, _handle: &CheckHandle, _report: &PipelineReport) -> Result<()> {
        Ok(())
    }

    async fn upload_report(&self, _report: &PipelineReport) -> Result<Option<String>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ContextReport, PipelineStatus};
    use std::collections::HashMap;

    fn make_test_report() -> PipelineReport {
        PipelineReport {
            version: "1.0".to_string(),
            project: "test".to_string(),
            pipeline: "ci".to_string(),
            context: ContextReport {
                provider: "local".to_string(),
                event: "manual".to_string(),
                ref_name: "current".to_string(),
                base_ref: None,
                sha: "abc123".to_string(),
                changed_files: vec![],
            },
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            duration_ms: Some(100),
            status: PipelineStatus::Success,
            tasks: vec![],
            annotations: HashMap::new(),
        }
    }

    #[test]
    fn test_local_provider_detect() {
        let provider = LocalProvider::detect();
        assert!(provider.is_some());

        let provider = provider.unwrap();
        let ctx = provider.context();
        assert_eq!(ctx.provider, "local");
        assert_eq!(ctx.event, "manual");
        assert_eq!(ctx.ref_name, "current");
        assert!(ctx.base_ref.is_none());
    }

    #[test]
    fn test_local_provider_with_base_ref() {
        let provider = LocalProvider::with_base_ref("main".to_string());
        let ctx = provider.context();

        assert_eq!(ctx.provider, "local");
        assert_eq!(ctx.event, "manual");
        assert_eq!(ctx.base_ref, Some("main".to_string()));
    }

    #[test]
    fn test_local_provider_context_sha() {
        let provider = LocalProvider::detect().unwrap();
        let ctx = provider.context();
        assert_eq!(ctx.sha, "current");
    }

    #[tokio::test]
    async fn test_local_provider_create_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = provider.create_check("test-check").await.unwrap();
        assert_eq!(handle.id, "local");
    }

    #[tokio::test]
    async fn test_local_provider_update_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = CheckHandle {
            id: "local".to_string(),
        };
        let result = provider.update_check(&handle, "running").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_local_provider_complete_check() {
        let provider = LocalProvider::detect().unwrap();
        let handle = CheckHandle {
            id: "local".to_string(),
        };
        let report = make_test_report();
        let result = provider.complete_check(&handle, &report).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_local_provider_upload_report_returns_none() {
        let provider = LocalProvider::detect().unwrap();
        let report = make_test_report();
        let result = provider.upload_report(&report).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_local_provider_changed_files() {
        // This test is dependent on git, but should work in any repo
        let provider = LocalProvider::detect().unwrap();
        let result = provider.changed_files().await;
        // Should succeed even if there are no changes
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_local_provider_with_base_ref_changed_files() {
        // Test with a base ref that exists
        let provider = LocalProvider::with_base_ref("HEAD~1".to_string());
        let result = provider.changed_files().await;
        // Should succeed even if the range is invalid
        assert!(result.is_ok());
    }
}
