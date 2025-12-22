//! CODEOWNERS sync provider.
//!
//! Syncs CODEOWNERS file from CUE configuration.
//! CODEOWNERS is always aggregated from all configs in the workspace.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Sync provider for CODEOWNERS.
pub struct CodeOwnersSyncProvider;

#[async_trait]
impl SyncProvider for CodeOwnersSyncProvider {
    fn name(&self) -> &'static str {
        "codeowners"
    }

    fn description(&self) -> &'static str {
        "Sync CODEOWNERS file (always aggregates all configs)"
    }

    fn has_config(&self, manifest: &Base) -> bool {
        manifest.owners.is_some()
    }

    async fn sync_path(
        &self,
        _path: &Path,
        package: &str,
        options: &SyncOptions,
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        // CODEOWNERS always uses workspace aggregation since it's a single file at repo root
        self.sync_workspace(package, options, _executor).await
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let output = functions::execute_sync_codeowners_workspace(package, dry_run, check).await?;

        Ok(SyncResult::success(output))
    }
}
