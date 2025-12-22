//! CI workflow sync provider.
//!
//! Syncs CI workflow files (GitHub Actions, Buildkite) from CUE configuration.

use async_trait::async_trait;
use clap::{Arg, Command, arg};
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
        self.default_command()
            .arg(arg!(--force "Overwrite existing workflow files"))
            .arg(
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
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let output = functions::execute_sync_ci(
            path.to_str().unwrap_or("."),
            package,
            dry_run,
            check,
            options.force,
            options.ci_provider.as_deref(),
        )
        .await?;

        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        _executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let output = functions::execute_sync_ci_workspace(
            package,
            dry_run,
            check,
            options.force,
            options.ci_provider.as_deref(),
        )
        .await?;

        Ok(SyncResult::success(output))
    }
}
