//! CI workflow sync provider.
//!
//! Syncs CI workflow files (GitHub Actions, Buildkite) from CUE configuration.

use async_trait::async_trait;
use clap::{Arg, Command};
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Sync provider for CI workflows.
pub struct CiSyncProvider;

#[async_trait]
impl SyncProvider for CiSyncProvider {
    fn name(&self) -> &'static str {
        "ci"
    }

    fn description(&self) -> &'static str {
        "Sync CI workflow files (GitHub Actions, Buildkite)"
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // CI config is on Project, not Base
        // For simplicity, we'll check during sync
        false
    }

    fn build_command(&self) -> Command {
        self.default_command().arg(
            Arg::new("provider")
                .long("provider")
                .help("Filter to specific provider (github, buildkite)")
                .value_name("PROVIDER"),
        )
    }

    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        // If the provided path is the module root (not a specific project),
        // treat this as a workspace-wide sync to avoid spurious errors when
        // the repo root is a Base instance. To resolve the module root, load
        // (and cache) the module for this path first.
        let is_module_root = {
            use crate::commands::env_file::find_cue_module_root as find_root;
            let target_path = path
                .canonicalize()
                .unwrap_or_else(|_| path.to_path_buf());
            match find_root(&target_path) {
                Some(root) => root == target_path,
                None => false,
            }
        };

        if is_module_root {
            let ci_options = functions::CiSyncOptions {
                dry_run: dry_run.into(),
                check,
                provider: options.ci_provider.as_deref(),
            };
            let request = functions::CiWorkspaceSyncRequest { package, options: ci_options };
            let output = functions::execute_sync_ci_workspace(request, executor).await?;
            return Ok(SyncResult::success(output));
        }

        let ci_options = functions::CiSyncOptions {
            dry_run: dry_run.into(),
            check,
            provider: options.ci_provider.as_deref(),
        };
        let request = functions::CiSyncRequest {
            path: path.to_str().unwrap_or("."),
            package,
            options: ci_options,
        };
        let output = functions::execute_sync_ci(request, executor).await?;

        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let ci_options = functions::CiSyncOptions {
            dry_run: dry_run.into(),
            check,
            provider: options.ci_provider.as_deref(),
        };
        let request = functions::CiWorkspaceSyncRequest {
            package,
            options: ci_options,
        };
        let output = functions::execute_sync_ci_workspace(request, executor).await?;

        Ok(SyncResult::success(output))
    }
}
