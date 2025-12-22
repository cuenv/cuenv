//! Ignore file sync provider.
//!
//! Syncs ignore files (.gitignore, .dockerignore, etc.) from CUE configuration.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Sync provider for ignore files.
pub struct IgnoreSyncProvider;

#[async_trait]
impl SyncProvider for IgnoreSyncProvider {
    fn name(&self) -> &'static str {
        "ignore"
    }

    fn description(&self) -> &'static str {
        "Sync ignore files (.gitignore, .dockerignore, etc.)"
    }

    fn has_config(&self, manifest: &Base) -> bool {
        manifest.ignore.is_some()
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

        let output = functions::execute_sync_ignore(
            path.to_str().unwrap_or("."),
            package,
            dry_run,
            check,
            Some(executor),
        )
        .await?;

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

        // For ignore files, we iterate over all instances with ignore config
        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;

        // Collect paths with ignore config before any async operations
        let paths_to_sync: Vec<(std::path::PathBuf, String)> = {
            let module = executor.get_module(&cwd)?;
            let mut paths = Vec::new();
            for (path, instance) in &module.instances {
                if let Ok(manifest) = instance.deserialize::<Base>()
                    && manifest.ignore.is_some()
                {
                    paths.push((module.root.join(path), path.display().to_string()));
                }
            }
            paths
            // module guard is dropped here at the end of the block
        };

        let mut outputs = Vec::new();
        let mut had_error = false;

        for (full_path, display_path) in paths_to_sync {
            let result = functions::execute_sync_ignore(
                full_path.to_str().unwrap_or("."),
                package,
                dry_run,
                check,
                Some(executor),
            )
            .await;

            match result {
                Ok(output) if !output.is_empty() => {
                    let display = if display_path.is_empty() {
                        "[root]".to_string()
                    } else {
                        display_path
                    };
                    outputs.push(format!("{display}:\n{output}"));
                }
                Ok(_) => {}
                Err(e) => {
                    outputs.push(format!("{display_path}: Error: {e}"));
                    had_error = true;
                }
            }
        }

        if outputs.is_empty() {
            Ok(SyncResult::success("No ignore configurations found."))
        } else {
            Ok(SyncResult {
                output: outputs.join("\n\n"),
                had_error,
            })
        }
    }
}
