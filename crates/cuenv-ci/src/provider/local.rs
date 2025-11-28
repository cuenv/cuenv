use super::{CIContext, CIProvider};
use crate::report::{CheckHandle, PipelineReport};
use async_trait::async_trait;
use cuenv_core::Result;
use std::path::PathBuf;

pub struct LocalProvider {
    context: CIContext,
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
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<Vec<PathBuf>> {
        // In local mode, maybe we assume everything is "affected" or we check against master?
        // For now, let's return empty which means "run nothing" unless we implement
        // "run all" logic or "diff against main" logic for local.
        // Issue spec says: "Always affected: Only run tasks whose inputs changed".
        // So locally, maybe we want to see uncommitted changes?
        // "git diff --name-only HEAD" (staged+unstaged)

        let output = std::process::Command::new("git")
            .args(["diff", "--name-only", "HEAD"])
            .output()
            .ok();

        if let Some(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.lines().map(PathBuf::from).collect())
        } else {
            Ok(vec![])
        }
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
