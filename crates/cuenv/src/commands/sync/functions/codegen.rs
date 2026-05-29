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

    if !sync_result.drift_paths.is_empty() {
        let drift_list = sync_result
            .drift_paths
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(cuenv_core::Error::configuration(format!(
            "Generated files are out of sync. Run 'cuenv sync codegen' to update:\n{drift_list}"
        )));
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
    /// Paths that check mode found missing or stale. Empty unless drift was
    /// detected (which only happens in check mode). The check-mode error is
    /// built directly from these paths, so it never has to re-parse `output`.
    drift_paths: Vec<String>,
}

/// Result of syncing a single codegen file.
///
/// `Written` and `Drift` are mutually exclusive: drift is only ever detected in
/// check/diff mode (where nothing is written), and writes only happen outside
/// those modes. Modelling this as an enum keeps the impossible
/// "written and drifted" state unrepresentable.
enum FileSyncOutcome {
    /// The file was written to disk.
    Written,
    /// Check/diff mode found the file missing or stale.
    Drift,
    /// Nothing to do (already in sync, scaffold exists, or dry-run preview).
    Unchanged,
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
    let mut drift_paths = Vec::new();

    for (file_path, file_def) in &codegen_config.files {
        let output_path = project_root.join(file_path);
        validate_lint_config(file_path, file_def)?;
        let content = format_codegen_content(file_def)?;
        let mut file_request = CodegenFileSyncRequest {
            output_lines: &mut output_lines,
            output_path: &output_path,
            file_path,
            content: &content,
        };

        let outcome = match file_def.mode {
            FileMode::Managed => sync_managed_file(&mut file_request, options)?,
            FileMode::Scaffold => sync_scaffold_file(&mut file_request, options)?,
        };
        match outcome {
            FileSyncOutcome::Written => written_files.push(output_path),
            FileSyncOutcome::Drift => drift_paths.push(file_path.clone()),
            FileSyncOutcome::Unchanged => {}
        }
    }

    drift_paths.extend(sync_gitignore_entries(
        &mut output_lines,
        project_root,
        codegen_config,
        options,
    )?);

    tracing::info!(
        project = project_name,
        files = codegen_config.files.len(),
        written = written_files.len(),
        "Codegen sync complete"
    );

    Ok(SyncResult {
        output: output_lines.join("\n"),
        written_files,
        drift_paths,
    })
}

/// Sync a managed codegen file (always overwritten to match expected content)
///
/// Returns whether the file was written or drift was detected.
fn sync_managed_file(
    request: &mut CodegenFileSyncRequest<'_>,
    options: CodegenSyncOptions,
) -> Result<FileSyncOutcome> {
    if options.should_check() {
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
                Ok(FileSyncOutcome::Unchanged)
            } else {
                request
                    .output_lines
                    .push(format!("  Out of sync: {}", request.file_path));
                maybe_push_diff(request, Some(&contents), options);
                Ok(FileSyncOutcome::Drift)
            }
        } else {
            request
                .output_lines
                .push(format!("  Missing: {}", request.file_path));
            maybe_push_diff(request, None, options);
            Ok(FileSyncOutcome::Drift)
        }
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
        Ok(FileSyncOutcome::Unchanged)
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
        Ok(FileSyncOutcome::Written)
    }
}

/// Sync a scaffold codegen file (only created if it doesn't exist)
///
/// Returns whether the file was written or drift was detected.
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
        Ok(FileSyncOutcome::Unchanged)
    } else if options.should_check() {
        request
            .output_lines
            .push(format!("  Missing scaffold: {}", request.file_path));
        maybe_push_diff(request, None, options);
        Ok(FileSyncOutcome::Drift)
    } else if options.dry_run.is_dry_run() {
        request
            .output_lines
            .push(format!("  Would scaffold: {}", request.file_path));
        Ok(FileSyncOutcome::Unchanged)
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
        Ok(FileSyncOutcome::Written)
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

fn format_codegen_content(file: &cuenv_core::manifest::ProjectFile) -> Result<String> {
    if file.language != "json" {
        return Ok(file.content.clone());
    }

    let value: serde_json::Value = serde_json::from_str(&file.content)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
    let indent_bytes = if file.format.indent == "tab" {
        b"\t".to_vec()
    } else {
        vec![b' '; file.format.indent_size.unwrap_or(2)]
    };
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent_bytes);
    let mut serializer = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&value, &mut serializer)
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| cuenv_core::Error::configuration(e.to_string()))
}

/// Validate generated content when the file opts into linting.
///
/// Linting is best-effort: JSON content is checked for valid syntax, and any
/// other language is accepted as a no-op until a validator exists for it. The
/// schema permits `lint.enabled` on every file type, so an unsupported language
/// must not be a hard failure.
fn validate_lint_config(file_path: &str, file: &cuenv_core::manifest::ProjectFile) -> Result<()> {
    let Some(lint) = file.lint.as_ref() else {
        return Ok(());
    };
    if !lint.enabled {
        return Ok(());
    }

    match file.language.as_str() {
        "json" => serde_json::from_str::<serde_json::Value>(&file.content)
            .map(|_| ())
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Lint failed for {file_path}: {e}"))
            }),
        other => {
            tracing::debug!(
                file = file_path,
                language = other,
                "Lint enabled but no validator for this language; skipping"
            );
            Ok(())
        }
    }
}

/// Sync the `cuenv codegen` `.gitignore` section, returning any paths that
/// drifted. Drift is only reported in check mode (dry-run previews are not
/// failures), mirroring the per-file drift semantics.
fn sync_gitignore_entries(
    output_lines: &mut Vec<String>,
    project_root: &Path,
    codegen_config: &cuenv_core::manifest::CodegenConfig,
    options: CodegenSyncOptions,
) -> Result<Vec<String>> {
    let patterns: Vec<String> = codegen_config
        .files
        .iter()
        .filter(|(_, file_def)| file_def.gitignore)
        .map(|(path, _)| path.clone())
        .collect();

    if patterns.is_empty() && !gitignore_has_codegen_section(project_root)? {
        return Ok(Vec::new());
    }

    let result = IgnoreFiles::builder()
        .directory(project_root)
        .dry_run(options.dry_run.is_dry_run() || options.should_check())
        .section(IgnoreSection::new("cuenv codegen").patterns(patterns))
        .generate()
        .map_err(|e| cuenv_core::Error::configuration(e.to_string()))?;

    let mut drift_paths = Vec::new();
    for file in result.files {
        match file.status {
            FileStatus::Created | FileStatus::Updated => {
                output_lines.push(format!("  Updated: {}", file.filename));
            }
            FileStatus::Unchanged => {
                output_lines.push(format!("  OK: {}", file.filename));
            }
            FileStatus::WouldCreate | FileStatus::WouldUpdate => {
                let action = if options.should_check() {
                    "Out of sync"
                } else {
                    "Would update"
                };
                output_lines.push(format!("  {action}: {}", file.filename));
                if options.should_check() {
                    drift_paths.push(file.filename);
                }
            }
        }
    }

    Ok(drift_paths)
}

fn gitignore_has_codegen_section(project_root: &Path) -> Result<bool> {
    let path = project_root.join(".gitignore");
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(content.lines().any(|line| line == "# BEGIN cuenv codegen")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(cuenv_core::Error::Io {
            source: e,
            path: Some(path.into_boxed_path()),
            operation: "read .gitignore".to_string(),
        }),
    }
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
    use cuenv_core::manifest::{CodegenConfig, FileMode, FormatConfig, LintConfig, ProjectFile};
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

        assert!(!result.drift_paths.is_empty());
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

        assert!(!result.drift_paths.is_empty());
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

        assert!(!result.drift_paths.is_empty());
        assert!(result.output.contains("Out of sync: .gitignore"));
    }

    #[test]
    fn check_mode_marks_stale_gitignore_section_as_drift() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp_dir.path().join(".gitignore"),
            "# BEGIN cuenv codegen\nold.txt\n# END cuenv codegen\n",
        )
        .expect("write gitignore");
        std::fs::write(temp_dir.path().join("tracked.txt"), "tracked").expect("write file");
        let codegen_config = config([("tracked.txt", project_file("tracked", "text"))]);
        let request = CodegenSyncFilesRequest {
            project_root: temp_dir.path(),
            project_name: "test",
            codegen_config: &codegen_config,
            options: options(true),
        };

        let result = sync_codegen_files(&request).expect("sync check");

        assert!(!result.drift_paths.is_empty());
        assert!(result.output.contains("Out of sync: .gitignore"));
    }

    #[test]
    fn enabled_json_lint_rejects_invalid_json_content() {
        let mut file = project_file("{ invalid json }", "json");
        file.lint = Some(LintConfig { enabled: true });

        let err = validate_lint_config("bad.json", &file).expect_err("lint should fail");

        assert!(err.to_string().contains("Lint failed for bad.json"));
    }

    #[test]
    fn enabled_lint_on_unsupported_language_is_a_noop() {
        let mut file = project_file("not really yaml", "yaml");
        file.lint = Some(LintConfig { enabled: true });

        validate_lint_config("config.yaml", &file).expect("unsupported lint should be a no-op");
    }
}
