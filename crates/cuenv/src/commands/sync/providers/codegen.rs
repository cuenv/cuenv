//! Codegen sync provider.
//!
//! Syncs codegen-generated files from CUE configuration.

use async_trait::async_trait;
use clap::{Command, arg};
use cuenv_core::Result;
use cuenv_core::manifest::{Base, Project};
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncProvider, SyncResult};

/// Sync provider for codegen.
pub struct CodegenSyncProvider;

#[async_trait]
impl SyncProvider for CodegenSyncProvider {
    fn name(&self) -> &'static str {
        "codegen"
    }

    fn description(&self) -> &'static str {
        "Sync files from CUE codegen configurations"
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // Codegen configs are only in Projects, not Base
        // We check if we can deserialize as Project with codegen config
        // For the trait, we accept Base but codegen won't be present
        false // Will be checked differently for projects
    }

    fn build_command(&self) -> Command {
        self.default_command()
            .arg(arg!(--diff "Show diff for files that would change"))
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

        let output = functions::execute_sync_codegen(
            path.to_str().unwrap_or("."),
            package,
            dry_run,
            check,
            options.show_diff,
            executor,
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

        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;

        // Collect project info before any async operations
        let project_paths: Vec<(std::path::PathBuf, String)> = {
            let module = executor.get_module(&cwd)?;
            let mut paths = Vec::new();
            for instance in module.projects() {
                if let Ok(manifest) = instance.deserialize::<Project>()
                    && manifest.codegen.is_some()
                {
                    paths.push((
                        module.root.join(&instance.path),
                        instance.path.display().to_string(),
                    ));
                }
            }
            paths
            // module guard is dropped here at the end of the block
        };

        let mut outputs = Vec::new();
        let mut had_error = false;

        // Iterate through projects with codegen config
        for (full_path, display_path) in project_paths {
            let result = functions::execute_sync_codegen(
                full_path.to_str().unwrap_or("."),
                package,
                dry_run,
                check,
                options.show_diff,
                executor,
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
            Ok(SyncResult::success("No codegen configurations found."))
        } else {
            Ok(SyncResult {
                output: outputs.join("\n\n"),
                had_error,
            })
        }
    }
}
