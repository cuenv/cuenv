//! Codegen sync provider.
//!
//! Syncs codegen-generated files from CUE configuration.

use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncProvider, SyncRequest, SyncResult, SyncScope};
use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_core::manifest::Project;

/// Sync provider for codegen.
pub struct CodegenSyncProvider;

#[async_trait]
impl SyncProvider for CodegenSyncProvider {
    fn name(&self) -> &'static str {
        "codegen"
    }

    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult> {
        match request.scope {
            SyncScope::Path => sync_path(request).await,
            SyncScope::Workspace => sync_workspace(request).await,
        }
    }
}

async fn sync_path(request: SyncRequest<'_>) -> Result<SyncResult> {
    let SyncRequest {
        path,
        package,
        options,
        executor,
        ..
    } = request;
    let dry_run = options.mode == SyncMode::DryRun;
    let check = options.mode == SyncMode::Check;

    let codegen_options = functions::CodegenSyncOptions {
        dry_run: dry_run.into(),
        check,
        diff: options.show_diff,
    };
    let request = functions::CodegenSyncRequest {
        path: path.to_str().unwrap_or("."),
        package,
        options: codegen_options,
    };
    let output = functions::execute_sync_codegen(request, executor).await?;

    Ok(SyncResult::success(output))
}

async fn sync_workspace(request: SyncRequest<'_>) -> Result<SyncResult> {
    let SyncRequest {
        path,
        package,
        options,
        executor,
        ..
    } = request;
    let dry_run = options.mode == SyncMode::DryRun;
    let check = options.mode == SyncMode::Check;

    // Collect project info before any async operations
    let project_paths: Vec<(std::path::PathBuf, String)> = {
        let module = executor.discover_all_modules(path)?;
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
        let codegen_options = functions::CodegenSyncOptions {
            dry_run: dry_run.into(),
            check,
            diff: options.show_diff,
        };
        let request = functions::CodegenSyncRequest {
            path: full_path.to_str().unwrap_or("."),
            package,
            options: codegen_options,
        };
        let result = functions::execute_sync_codegen(request, executor).await;

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
