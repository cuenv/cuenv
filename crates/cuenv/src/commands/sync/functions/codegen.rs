//! Codegen sync operations.

use crate::commands::sync::formatters;
use crate::commands::{CommandExecutor, relative_path_from_root};
use cuenv_core::manifest::Project;
use cuenv_core::{DryRun, Result};
use similar::TextDiff;
use std::path::{Path, PathBuf};
use tracing::instrument;

/// Options controlling codegen sync behavior.
#[derive(Clone, Copy, Debug)]
pub struct CodegenSyncOptions {
    /// Show what would be generated without writing files.
    pub dry_run: DryRun,
    /// Check if files are in sync without making changes.
    pub check: bool,
    /// Show diff for files that would change.
    pub diff: bool,
}

impl CodegenSyncOptions {
    fn should_check(self) -> bool {
        self.check || self.diff
    }
}

/// Request for syncing codegen for a single path.
#[derive(Debug)]
pub struct CodegenSyncRequest<'a> {
    /// Path to the CUE module or project directory.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Sync options.
    pub options: CodegenSyncOptions,
}

struct CodegenSyncContext<'a> {
    dir_path: &'a Path,
    options: CodegenSyncOptions,
    executor: &'a CommandExecutor,
}

struct CodegenSyncFilesRequest<'a> {
    project_root: &'a Path,
    project_name: &'a str,
    codegen_config: &'a cuenv_core::manifest::CodegenConfig,
    options: CodegenSyncOptions,
}

struct CodegenFileSyncRequest<'a> {
    output_lines: &'a mut Vec<String>,
    output_path: &'a Path,
    file_path: &'a str,
    content: &'a str,
}

struct CodegenWriteRequest<'a> {
    output_path: &'a Path,
    file_path: &'a str,
    content: &'a str,
    mode: &'a str,
}

/// Load Project configuration from CUE using module-wide evaluation.
fn load_project_config(path: &Path, executor: &CommandExecutor) -> Result<Project> {
    let (instance, _module_root) = load_instance_at_path(path, executor)?;
    instance.deserialize()
}

/// Load a CUE instance at the given path using module-wide evaluation.
/// Returns the instance and the module root path.
///
/// Uses the executor's cached module evaluation.
fn load_instance_at_path(
    path: &Path,
    executor: &CommandExecutor,
) -> Result<(cuenv_core::module::Instance, PathBuf)> {
    let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "canonicalize path".to_string(),
    })?;

    tracing::debug!("Using cached module evaluation from executor");
    let module = executor.get_module(&target_path)?;
    let relative_path = relative_path_from_root(&module.root, &target_path);

    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    Ok((instance.clone(), module.root.clone()))
}

/// Execute the sync codegen command for a single project.
///
/// Syncs codegen-generated files for the project at the specified path.
///
/// Uses the executor's cached module evaluation.
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or file operations fail.
#[instrument(name = "sync_codegen", skip(executor))]
pub async fn execute_sync_codegen(
    request: CodegenSyncRequest<'_>,
    executor: &CommandExecutor,
) -> Result<String> {
    tracing::info!("Starting sync codegen command");

    let dir_path = Path::new(request.path);
    let context = CodegenSyncContext {
        dir_path,
        options: request.options,
        executor,
    };
    execute_sync_codegen_local(&context)
}

/// Sync codegen for the local project only
fn execute_sync_codegen_local(context: &CodegenSyncContext<'_>) -> Result<String> {
    let dir_path = context.dir_path;
    let options = context.options;
    let executor = context.executor;
    let manifest: Project = load_project_config(dir_path, executor)?;

    let Some(codegen_config) = &manifest.codegen else {
        return Ok("No codegen configuration found in this project.".to_string());
    };

    let sync_request = CodegenSyncFilesRequest {
        project_root: dir_path,
        project_name: &manifest.name,
        codegen_config,
        options,
    };
    let sync_result = sync_codegen_files(&sync_request)?;

    // Run formatters only on files that were actually written
    let format_result = if let Some(ref formatters_config) = manifest.formatters {
        if sync_result.written_files.is_empty() && !options.dry_run.is_dry_run() {
            // No files were written, skip formatting
            String::new()
        } else if options.dry_run.is_dry_run() {
            // In dry-run mode, show what would be formatted based on all configured files
            let file_paths: Vec<std::path::PathBuf> = codegen_config
                .files
                .keys()
                .map(|p| dir_path.join(p))
                .collect();
            let file_refs: Vec<&Path> = file_paths.iter().map(|p| p.as_path()).collect();
            formatters::format_generated_files(
                &file_refs,
                formatters_config,
                dir_path,
                options.dry_run,
                options.check,
            )?
        } else {
            // Format only the files that were actually written
            let file_refs: Vec<&Path> = sync_result
                .written_files
                .iter()
                .map(|p| p.as_path())
                .collect();
            formatters::format_generated_files(
                &file_refs,
                formatters_config,
                dir_path,
                options.dry_run,
                options.check,
            )?
        }
    } else {
        String::new()
    };

    // Combine results
    if format_result.is_empty() {
        Ok(sync_result.output)
    } else {
        Ok(format!("{}\n\n{format_result}", sync_result.output))
    }
}

/// Result of a codegen sync operation
struct SyncResult {
    /// Human-readable output
    output: String,
    /// Files that were actually written to disk
    written_files: Vec<PathBuf>,
}

/// Sync codegen files for a single project
fn sync_codegen_files(request: &CodegenSyncFilesRequest<'_>) -> Result<SyncResult> {
    use cuenv_core::manifest::FileMode;

    let project_root = request.project_root;
    let project_name = request.project_name;
    let codegen_config = request.codegen_config;
    let options = request.options;

    let mut output_lines = Vec::new();
    let mut written_files = Vec::new();

    for (file_path, file_def) in &codegen_config.files {
        let output_path = project_root.join(file_path);
        let mut file_request = CodegenFileSyncRequest {
            output_lines: &mut output_lines,
            output_path: &output_path,
            file_path,
            content: &file_def.content,
        };

        match file_def.mode {
            FileMode::Managed => {
                let was_written = sync_managed_file(&mut file_request, options)?;
                if was_written {
                    written_files.push(output_path);
                }
            }
            FileMode::Scaffold => {
                let was_written = sync_scaffold_file(&mut file_request, options)?;
                if was_written {
                    written_files.push(output_path);
                }
            }
        }
    }

    tracing::info!(
        project = project_name,
        files = codegen_config.files.len(),
        written = written_files.len(),
        "Codegen sync complete"
    );

    Ok(SyncResult {
        output: output_lines.join("\n"),
        written_files,
    })
}

/// Sync a managed codegen file (always overwritten to match expected content)
///
/// Returns `true` if the file was actually written to disk.
fn sync_managed_file(
    request: &mut CodegenFileSyncRequest<'_>,
    options: CodegenSyncOptions,
) -> Result<bool> {
    if options.should_check() {
        if request.output_path.exists() {
            let contents = std::fs::read_to_string(request.output_path).unwrap_or_default();
            if contents == request.content {
                request
                    .output_lines
                    .push(format!("  OK: {}", request.file_path));
            } else {
                request
                    .output_lines
                    .push(format!("  Out of sync: {}", request.file_path));
                maybe_push_diff(request, Some(&contents), options);
            }
        } else {
            request
                .output_lines
                .push(format!("  Missing: {}", request.file_path));
            maybe_push_diff(request, None, options);
        }
        Ok(false)
    } else if options.dry_run.is_dry_run() {
        if request.output_path.exists() {
            request
                .output_lines
                .push(format!("  Would update: {}", request.file_path));
        } else {
            request
                .output_lines
                .push(format!("  Would create: {}", request.file_path));
        }
        Ok(false)
    } else {
        let write_request = CodegenWriteRequest {
            output_path: request.output_path,
            file_path: request.file_path,
            content: request.content,
            mode: "managed",
        };
        write_codegen_file(&write_request)?;
        request
            .output_lines
            .push(format!("  Generated: {}", request.file_path));
        Ok(true)
    }
}

/// Sync a scaffold codegen file (only created if it doesn't exist)
///
/// Returns `true` if the file was actually written to disk.
fn sync_scaffold_file(
    request: &mut CodegenFileSyncRequest<'_>,
    options: CodegenSyncOptions,
) -> Result<bool> {
    if request.output_path.exists() {
        if !options.dry_run.is_dry_run() && !options.should_check() {
            tracing::debug!(
                "Skipping {} (scaffold mode, file exists)",
                request.file_path
            );
        }
        request
            .output_lines
            .push(format!("  Skipped (exists): {}", request.file_path));
        Ok(false)
    } else if options.should_check() {
        request
            .output_lines
            .push(format!("  Missing scaffold: {}", request.file_path));
        maybe_push_diff(request, None, options);
        Ok(false)
    } else if options.dry_run.is_dry_run() {
        request
            .output_lines
            .push(format!("  Would scaffold: {}", request.file_path));
        Ok(false)
    } else {
        let write_request = CodegenWriteRequest {
            output_path: request.output_path,
            file_path: request.file_path,
            content: request.content,
            mode: "scaffold",
        };
        write_codegen_file(&write_request)?;
        request
            .output_lines
            .push(format!("  Scaffolded: {}", request.file_path));
        Ok(true)
    }
}

/// Write a codegen file to disk, creating parent directories as needed
fn write_codegen_file(request: &CodegenWriteRequest<'_>) -> Result<()> {
    let CodegenWriteRequest {
        output_path,
        file_path,
        content,
        mode,
    } = *request;
    tracing::debug!(
        file_path = %file_path,
        output_path = %output_path.display(),
        content_len = content.len(),
        "Writing {mode} codegen file"
    );
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            tracing::error!(
                parent = %parent.display(),
                error = %e,
                "Failed to create parent directory"
            );
            cuenv_core::Error::Io {
                source: e,
                path: Some(parent.to_path_buf().into_boxed_path()),
                operation: format!("create parent directory for {mode} file: {file_path}"),
            }
        })?;
    }
    std::fs::write(output_path, content).map_err(|e| {
        tracing::error!(
            output_path = %output_path.display(),
            error = %e,
            "Failed to write {mode} file"
        );
        cuenv_core::Error::Io {
            source: e,
            path: Some(output_path.to_path_buf().into_boxed_path()),
            operation: format!("write {mode} file: {file_path}"),
        }
    })?;
    Ok(())
}

fn maybe_push_diff(
    request: &mut CodegenFileSyncRequest<'_>,
    existing: Option<&str>,
    options: CodegenSyncOptions,
) {
    if !options.diff {
        return;
    }
    let current = existing.unwrap_or("");
    if current == request.content {
        return;
    }
    request.output_lines.push(format_unified_diff(
        request.file_path,
        current,
        request.content,
    ));
}

fn format_unified_diff(path: &str, current: &str, expected: &str) -> String {
    let diff = TextDiff::from_lines(current, expected);
    let from = format!("a/{path}");
    let to = format!("b/{path}");
    diff.unified_diff().header(&from, &to).to_string()
}
