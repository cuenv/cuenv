//! CI workflow sync provider.
//!
//! Syncs CI workflow files (GitHub Actions, Buildkite) from CUE configuration.

use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncProvider, SyncRequest, SyncResult, SyncScope};
use async_trait::async_trait;
use cuenv_core::Result;

/// Sync provider for CI workflows.
pub struct CiSyncProvider;

#[async_trait]
impl SyncProvider for CiSyncProvider {
    fn name(&self) -> &'static str {
        "ci"
    }

    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult> {
        let SyncRequest {
            path,
            package,
            options,
            scope,
            executor,
        } = request;
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let ci_options = functions::CiSyncOptions {
            dry_run: dry_run.into(),
            check,
            provider: options.ci_provider.as_deref(),
        };
        let output = match scope {
            SyncScope::Path => {
                let request = functions::CiSyncRequest {
                    path: path.to_str().unwrap_or("."),
                    package,
                    options: ci_options,
                };
                functions::execute_sync_ci(request, executor).await?
            }
            SyncScope::Workspace => {
                let request = functions::CiWorkspaceSyncRequest {
                    path,
                    package,
                    options: ci_options,
                };
                functions::execute_sync_ci_workspace(request, executor).await?
            }
        };

        Ok(SyncResult::success(output))
    }
}
