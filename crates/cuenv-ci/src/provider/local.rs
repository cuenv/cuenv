use super::{CIContext, CIProvider};
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::collections::HashSet;
use std::path::PathBuf;

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
        let mut changed: HashSet<PathBuf> = HashSet::new();

        // If we have a base_ref, get committed changes since that ref
        if let Some(ref base_ref) = self.base_ref {
            // Use three-dot syntax to get changes between base_ref and HEAD
            // This shows what's in HEAD that isn't in base_ref
            let output = std::process::Command::new("git")
                .args(["diff", "--name-only", &format!("{base_ref}...HEAD")])
                .output()
                .ok();

            if let Some(output) = output {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    changed.insert(PathBuf::from(line));
                }
            }
        }

        // Always include uncommitted changes (staged + unstaged)
        let output = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .output()
            .ok();

        if let Some(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                changed.insert(PathBuf::from(line));
            }
        }

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
