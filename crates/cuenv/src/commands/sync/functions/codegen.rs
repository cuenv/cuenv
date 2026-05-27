//! Codegen sync operations.

use crate::commands::sync::formatters;
use crate::commands::{CommandExecutor, relative_path_from_root};
use cuenv_core::manifest::Project;
use cuenv_core::{DryRun, Result};
use cuenv_ignore::{FileStatus, IgnoreFiles, IgnoreSection};
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

    if options.should_check() && sync_result.had_drift {
        return Err(cuenv_core::Error::configuration(sync_result.output));
    }

    // Run formatters on files that were actually written, or all configured files in check/dry-run.
    let format_result = if let Some(ref formatters_config) = manifest.formatters {
        if sync_result.written_files.is_empty()
            && !options.dry_run.is_dry_run()
            && !options.should_check()
        {
            // No files were written, skip formatting
            String::new()
        } else if options.dry_run.is_dry_run() || options.should_check() {
            // In dry-run and check mode, inspect all configured files.
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
    /// Whether check mode found missing or stale generated state.
    had_drift: bool,
}

struct FileSyncOutcome {
    written: bool,
    drift: bool,
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
    let mut had_drift = false;

    for (file_path, file_def) in &codegen_config.files {
        let output_path = project_root.join(file_path);
        validate_lint_config(
            file_path,
            &file_def.language,
            &file_def.content,
            file_def.lint.as_ref(),
        )?;
        let content =
            format_codegen_content(&file_def.content, &file_def.language, &file_def.format)?;
        let mut file_request = CodegenFileSyncRequest {
            output_lines: &mut output_lines,
            output_path: &output_path,
            file_path,
            content: &content,
        };

        match file_def.mode {
            FileMode::Managed => {
                let outcome = sync_managed_file(&mut file_request, options)?;
                had_drift |= outcome.drift;
                if outcome.written {
                    written_files.push(output_path);
                }
            }
            FileMode::Scaffold => {
                let outcome = sync_scaffold_file(&mut file_request, options)?;
                had_drift |= outcome.drift;
                if outcome.written {
                    written_files.push(output_path);
                }
            }
        }
    }

    had_drift |= sync_gitignore_entries(&mut output_lines, project_root, codegen_config, options)?;

    tracing::info!(
        project = project_name,
        files = codegen_config.files.len(),
        written = written_files.len(),
        "Codegen sync complete"
    );

    Ok(SyncResult {
        output: output_lines.join("\n"),
        written_files,
        had_drift,
    })
}

/// Sync a managed codegen file (always overwritten to match expected content)
///
/// Returns `true` if the file was actually written to disk.
fn sync_managed_file(
    request: &mut CodegenFileSyncRequest<'_>,
    options: CodegenSyncOptions,
) -> Result<FileSyncOutcome> {
    if options.should_check() {
        let mut drift = false;
        if request.output_path.exists() {
            let contents = std::fs::read_to_string(request.output_path).map_err(|e| {
                cuenv_core::Error::Io {
                    source: e,
                    path: Some(request.output_path.to_path_buf().into_boxed_path()),
                    operation: "read generated file".to_string(),
                }
            })?;
            if contents == request.content {
                request
                    .output_lines
                    .push(format!("  OK: {}", request.file_path));
            } else {
                request
                    .output_lines
                    .push(format!("  Out of sync: {}", request.file_path));
                maybe_push_diff(request, Some(&contents), options);
                drift = true;
            }
        } else {
            request
                .output_lines
                .push(format!("  Missing: {}", request.file_path));
            maybe_push_diff(request, None, options);
            drift = true;
        }
        Ok(FileSyncOutcome {
            written: false,
            drift,
        })
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
        Ok(FileSyncOutcome {
            written: false,
            drift: false,
        })
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
        Ok(FileSyncOutcome {
            written: true,
            drift: false,
        })
    }
}

/// Sync a scaffold codegen file (only created if it doesn't exist)
///
/// Returns `true` if the file was actually written to disk.
fn sync_scaffold_file(
    request: &mut CodegenFileSyncRequest<'_>,
    options: CodegenSyncOptions,
) -> Result<FileSyncOutcome> {
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
        Ok(FileSyncOutcome {
            written: false,
            drift: false,
        })
    } else if options.should_check() {
        request
            .output_lines
            .push(format!("  Missing scaffold: {}", request.file_path));
        maybe_push_diff(request, None, options);
        Ok(FileSyncOutcome {
            written: false,
            drift: true,
        })
    } else if options.dry_run.is_dry_run() {
        request
            .output_lines
            .push(format!("  Would scaffold: {}", request.file_path));
        Ok(FileSyncOutcome {
            written: false,
            drift: false,
        })
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
        Ok(FileSyncOutcome {
            written: true,
            drift: false,
        })
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

fn format_codegen_content(
    content: &str,
    language: &str,
    format: &cuenv_core::manifest::FormatConfig,
) -> Result<String> {
    if language != "json" {
        return Ok(content.to_string());
    }

    let value: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
    let indent_bytes = if format.indent == "tab" {
        b"\t".to_vec()
    } else {
        vec![b' '; format.indent_size.unwrap_or(2)]
    };
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent_bytes);
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&value, &mut serializer)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| cuenv_core::Error::configuration(e.to_string()))
}

fn validate_lint_config(
    file_path: &str,
    language: &str,
    content: &str,
    lint: Option<&cuenv_core::manifest::LintConfig>,
) -> Result<()> {
    let Some(lint) = lint else {
        return Ok(());
    };
    if !lint.enabled {
        return Ok(());
    }

    match language {
        "json" => serde_json::from_str::<serde_json::Value>(content)
            .map(|_| ())
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Lint failed for {file_path}: {e}"))
            }),
        _ => Err(cuenv_core::Error::configuration(format!(
            "Linting is not supported for generated {language} files: {file_path}"
        ))),
    }
}

fn sync_gitignore_entries(
    output_lines: &mut Vec<String>,
    project_root: &Path,
    codegen_config: &cuenv_core::manifest::CodegenConfig,
    options: CodegenSyncOptions,
) -> Result<bool> {
    let patterns: Vec<String> = codegen_config
        .files
        .iter()
        .filter(|(_, file_def)| file_def.gitignore)
        .map(|(path, _)| path.clone())
        .collect();

    if patterns.is_empty() {
        return Ok(false);
    }

    let result = IgnoreFiles::builder()
        .directory(project_root)
        .dry_run(options.dry_run.is_dry_run() || options.should_check())
        .section(IgnoreSection::new("cuenv codegen").patterns(patterns))
        .generate()
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;

    let mut had_drift = false;
    for file in result.files {
        match file.status {
            FileStatus::Created | FileStatus::Updated => {
                output_lines.push(format!("  Updated: {}", file.filename));
            }
            FileStatus::Unchanged => {
                output_lines.push(format!("  OK: {}", file.filename));
            }
            FileStatus::WouldCreate | FileStatus::WouldUpdate => {
                had_drift = true;
                let action = if options.should_check() {
                    "Out of sync"
                } else {
                    "Would update"
                };
                output_lines.push(format!("  {action}: {}", file.filename));
            }
        }
    }

    Ok(had_drift && options.should_check())
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

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::{
        CodegenConfig, FileMode, FormatConfig, LintConfig, ProjectFile,
    };
    use std::collections::HashMap;

    fn options(check: bool) -> CodegenSyncOptions {
        CodegenSyncOptions {
            dry_run: DryRun::No,
            check,
            diff: false,
        }
    }

    fn project_file(content: &str, language: &str) -> ProjectFile {
        ProjectFile {
            content: content.to_string(),
            language: language.to_string(),
            mode: FileMode::Managed,
            format: FormatConfig::default(),
            gitignore: false,
            lint: None,
        }
    }

    fn config(files: impl IntoIterator<Item = (&'static str, ProjectFile)>) -> CodegenConfig {
        CodegenConfig {
            files: files
                .into_iter()
                .map(|(path, file)| (path.to_string(), file))
                .collect::<HashMap<_, _>>(),
            context: serde_json::Value::Null,
        }
    }

    #[test]
    fn check_mode_marks_missing_managed_file_as_drift() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let codegen_config = config([("generated.json", project_file(r#"{"ok":true}"#, "json"))]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(true),
        };

        let result = sync_codegen_files(&request).expect("sync check");

        assert!(result.had_drift);
        assert!(result.output.contains("Missing: generated.json"));
    }

    #[test]
    fn check_mode_marks_stale_managed_file_as_drift() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp_dir.path().join("generated.txt"), "old").expect("write stale file");
        let codegen_config = config([("generated.txt", project_file("new", "text"))]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(true),
        };

        let result = sync_codegen_files(&request).expect("sync check");

        assert!(result.had_drift);
        assert!(result.output.contains("Out of sync: generated.txt"));
    }

    #[test]
    fn write_mode_formats_json_content_with_file_format_config() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut file = project_file(r#"{"name":"test"}"#, "json");
        file.format.indent_size = Some(4);
        let codegen_config = config([("generated.json", file)]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(false),
        };

        sync_codegen_files(&request).expect("sync write");
        let content =
            std::fs::read_to_string(temp_dir.path().join("generated.json")).expect("read json");

        assert!(content.contains("\n    \"name\""));
    }

    #[test]
    fn write_mode_updates_gitignore_for_enabled_files() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut ignored = project_file("ignored", "text");
        ignored.gitignore = true;
        let tracked = project_file("tracked", "text");
        let codegen_config = config([("ignored.txt", ignored), ("tracked.txt", tracked)]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(false),
        };

        sync_codegen_files(&request).expect("sync write");
        let gitignore =
            std::fs::read_to_string(temp_dir.path().join(".gitignore")).expect("read gitignore");

        assert!(gitignore.contains("# BEGIN cuenv codegen"));
        assert!(gitignore.contains("ignored.txt"));
        assert!(!gitignore.contains("tracked.txt"));
    }

    #[test]
    fn check_mode_marks_missing_gitignore_entry_as_drift() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut ignored = project_file("ignored", "text");
        ignored.gitignore = true;
        std::fs::write(temp_dir.path().join("ignored.txt"), "ignored").expect("write file");
        let codegen_config = config([("ignored.txt", ignored)]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(true),
        };

        let result = sync_codegen_files(&request).expect("sync check");

        assert!(result.had_drift);
        assert!(result.output.contains("Out of sync: .gitignore"));
    }

    #[test]
    fn enabled_json_lint_rejects_invalid_json_content() {
        let mut file = project_file("{ invalid json }", "json");
        file.lint = Some(LintConfig {
            enabled: true,
            rules: serde_json::Value::Null,
        });

        let err = validate_lint_config("bad.json", &file.language, &file.content, file.lint.as_ref())
            .expect_err("lint should fail");

        assert!(err.to_string().contains("Lint failed for bad.json"));
    }
}
