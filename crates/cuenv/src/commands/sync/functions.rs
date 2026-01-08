//! Sync function implementations.
//!
//! Supports generating:
//! - Project files from CUE codegen templates
//! - CI pipelines from CUE configuration
//!
//! Note: Ignore files and CODEOWNERS are now handled via .rules.cue files.
//! Use `cuenv sync rules` for those.

use super::super::env_file::find_cue_module_root;
use super::super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result};
use cuenv_github::GitHubConfigExt;
use similar::TextDiff;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::instrument;

/// Project information for CI sync operations.
///
/// This is a local struct that holds the data needed for workflow generation,
/// derived from `ModuleEvaluation` and `Instance`.
struct ProjectInfo {
    /// Absolute path to the project directory.
    project_path: PathBuf,
    /// Relative path from module root to project directory.
    relative_path: PathBuf,
    /// Module root path.
    module_root: PathBuf,
    /// Parsed project configuration.
    config: Project,
}

impl ProjectInfo {
    /// Collect all projects from a module evaluation.
    fn collect_from_module(module: &ModuleEvaluation) -> Result<Vec<Self>> {
        let mut projects = Vec::new();
        for instance in module.projects() {
            let config = Project::try_from(instance)?;
            // instance.path is the relative path to the project directory (not env.cue)
            let relative_path = instance.path.clone();
            let project_path = module.root.join(&relative_path);
            projects.push(Self {
                project_path,
                relative_path,
                module_root: module.root.clone(),
                config,
            });
        }
        Ok(projects)
    }
}

/// Load Project configuration from CUE using module-wide evaluation.
fn load_project_config(
    path: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Project> {
    let (instance, _module_root) = load_instance_at_path(path, package, executor)?;
    instance.deserialize()
}

/// Load a CUE instance at the given path using module-wide evaluation.
/// Returns the instance and the module root path.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
fn load_instance_at_path(
    path: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<(cuenv_core::module::Instance, PathBuf)> {
    let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(path.to_path_buf().into_boxed_path()),
        operation: "canonicalize path".to_string(),
    })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let relative_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&relative_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                relative_path.display()
            ))
        })?;

        return Ok((instance.clone(), module.root.clone()));
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
        None,
    );

    let relative_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    Ok((instance.clone(), module_root))
}

/// Execute the sync codegen command for a single project.
///
/// Syncs codegen-generated files for the project at the specified path.
/// Use `execute_sync_codegen_workspace` for workspace-wide syncing.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or file operations fail.
#[instrument(name = "sync_codegen", skip(executor))]
pub async fn execute_sync_codegen(
    path: &str,
    package: &str,
    dry_run: bool,
    check: bool,
    diff: bool,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    tracing::info!("Starting sync codegen command");

    let dir_path = Path::new(path);
    execute_sync_codegen_local(dir_path, package, dry_run, check, diff, executor)
}

/// Sync codegen for the local project only
fn execute_sync_codegen_local(
    dir_path: &Path,
    package: &str,
    dry_run: bool,
    check: bool,
    diff: bool,
    executor: Option<&CommandExecutor>,
) -> Result<String> {
    // Auto-detect package name from env.cue if using default
    let effective_package = if package == "cuenv" {
        detect_package_name(dir_path)?
    } else {
        package.to_string()
    };

    // Use module-wide evaluation (cached if executor provided)
    let manifest: Project = load_project_config(dir_path, &effective_package, executor)?;

    let Some(codegen_config) = &manifest.codegen else {
        return Ok("No codegen configuration found in this project.".to_string());
    };

    let sync_result = sync_codegen_files(
        dir_path,
        &manifest.name,
        codegen_config,
        dry_run,
        check,
        diff,
    )?;

    // Run formatters only on files that were actually written
    let format_result = if let Some(ref formatters) = manifest.formatters {
        if sync_result.written_files.is_empty() && !dry_run {
            // No files were written, skip formatting
            String::new()
        } else if dry_run {
            // In dry-run mode, show what would be formatted based on all configured files
            let file_paths: Vec<std::path::PathBuf> = codegen_config
                .files
                .keys()
                .map(|p| dir_path.join(p))
                .collect();
            let file_refs: Vec<&Path> = file_paths.iter().map(|p| p.as_path()).collect();
            super::formatters::format_generated_files(
                &file_refs, formatters, dir_path, dry_run, check,
            )?
        } else {
            // Format only the files that were actually written
            let file_refs: Vec<&Path> = sync_result
                .written_files
                .iter()
                .map(|p| p.as_path())
                .collect();
            super::formatters::format_generated_files(
                &file_refs, formatters, dir_path, dry_run, check,
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
fn sync_codegen_files(
    project_root: &Path,
    project_name: &str,
    codegen_config: &cuenv_core::manifest::CodegenConfig,
    dry_run: bool,
    check: bool,
    diff: bool,
) -> Result<SyncResult> {
    use cuenv_core::manifest::FileMode;

    let mut output_lines = Vec::new();
    let mut written_files = Vec::new();

    for (file_path, file_def) in &codegen_config.files {
        let output_path = project_root.join(file_path);

        match file_def.mode {
            FileMode::Managed => {
                let was_written = sync_managed_file(
                    &mut output_lines,
                    &output_path,
                    file_path,
                    &file_def.content,
                    dry_run,
                    check,
                    diff,
                )?;
                if was_written {
                    written_files.push(output_path);
                }
            }
            FileMode::Scaffold => {
                let was_written = sync_scaffold_file(
                    &mut output_lines,
                    &output_path,
                    file_path,
                    &file_def.content,
                    dry_run,
                    check,
                    diff,
                )?;
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
    output_lines: &mut Vec<String>,
    output_path: &Path,
    file_path: &str,
    content: &str,
    dry_run: bool,
    check: bool,
    diff: bool,
) -> Result<bool> {
    if check || diff {
        if output_path.exists() {
            let contents = std::fs::read_to_string(output_path).unwrap_or_default();
            if contents == content {
                output_lines.push(format!("  OK: {file_path}"));
            } else {
                output_lines.push(format!("  Out of sync: {file_path}"));
                maybe_push_diff(output_lines, diff, file_path, Some(&contents), content);
            }
        } else {
            output_lines.push(format!("  Missing: {file_path}"));
            maybe_push_diff(output_lines, diff, file_path, None, content);
        }
        Ok(false)
    } else if dry_run {
        if output_path.exists() {
            output_lines.push(format!("  Would update: {file_path}"));
        } else {
            output_lines.push(format!("  Would create: {file_path}"));
        }
        Ok(false)
    } else {
        write_codegen_file(output_path, file_path, content, "managed")?;
        output_lines.push(format!("  Generated: {file_path}"));
        Ok(true)
    }
}

/// Sync a scaffold codegen file (only created if it doesn't exist)
///
/// Returns `true` if the file was actually written to disk.
fn sync_scaffold_file(
    output_lines: &mut Vec<String>,
    output_path: &Path,
    file_path: &str,
    content: &str,
    dry_run: bool,
    check: bool,
    diff: bool,
) -> Result<bool> {
    if output_path.exists() {
        if !dry_run && !check {
            tracing::debug!("Skipping {file_path} (scaffold mode, file exists)");
        }
        output_lines.push(format!("  Skipped (exists): {file_path}"));
        Ok(false)
    } else if check || diff {
        output_lines.push(format!("  Missing scaffold: {file_path}"));
        maybe_push_diff(output_lines, diff, file_path, None, content);
        Ok(false)
    } else if dry_run {
        output_lines.push(format!("  Would scaffold: {file_path}"));
        Ok(false)
    } else {
        write_codegen_file(output_path, file_path, content, "scaffold")?;
        output_lines.push(format!("  Scaffolded: {file_path}"));
        Ok(true)
    }
}

/// Write a codegen file to disk, creating parent directories as needed
fn write_codegen_file(
    output_path: &Path,
    file_path: &str,
    content: &str,
    mode: &str,
) -> Result<()> {
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
    output_lines: &mut Vec<String>,
    diff: bool,
    file_path: &str,
    existing: Option<&str>,
    expected: &str,
) {
    if !diff {
        return;
    }
    let current = existing.unwrap_or("");
    if current == expected {
        return;
    }
    output_lines.push(format_unified_diff(file_path, current, expected));
}

fn format_unified_diff(path: &str, current: &str, expected: &str) -> String {
    let diff = TextDiff::from_lines(current, expected);
    let from = format!("a/{path}");
    let to = format!("b/{path}");
    diff.unified_diff().header(&from, &to).to_string()
}

/// Detect the CUE package name from env.cue
fn detect_package_name(project_path: &Path) -> Result<String> {
    let env_cue = project_path.join("env.cue");
    if !env_cue.exists() {
        return Ok("cuenv".to_string());
    }

    let content = std::fs::read_to_string(&env_cue)?;
    for line in content.lines().take(10) {
        let trimmed = line.trim();
        if trimmed.starts_with("package ") {
            return Ok(trimmed
                .strip_prefix("package ")
                .unwrap_or("cuenv")
                .trim()
                .to_string());
        }
    }

    Ok("cuenv".to_string())
}

// ============================================================================
// CI Workflow Sync
// ============================================================================

/// Execute the sync ci command for a single project.
///
/// Syncs CI workflow files (GitHub Actions, Buildkite) based on CUE configuration.
///
/// # Errors
///
/// Returns an error if project discovery fails or workflow generation fails.
#[instrument(name = "sync_ci", skip_all)]
pub async fn execute_sync_ci(
    path: &str,
    _package: &str,
    dry_run: bool,
    check: bool,
    provider: Option<&str>,
    executor: &CommandExecutor,
) -> Result<String> {
    tracing::info!("Starting sync ci command");

    let dir_path = Path::new(path);

    // Get cached module from executor and discover projects before async work
    // (ModuleGuard contains MutexGuard which is not Send)
    let (projects, repo_root, target_path) = {
        let target_path = dir_path.canonicalize().map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(dir_path.to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;
        let module = executor.get_module(&target_path)?;
        let projects = ProjectInfo::collect_from_module(&module)?;
        (projects, module.root.clone(), target_path)
    };

    let target_projects: Vec<_> = projects
        .into_iter()
        .filter(|project| {
            // project_path is absolute path to project directory
            project
                .project_path
                .canonicalize()
                .ok()
                .is_some_and(|path| path == target_path)
        })
        .collect();

    if target_projects.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No cuenv project found at path: {}. Run 'cuenv info' to inspect project layout or use 'cuenv sync -A' to sync all projects.",
            dir_path.display()
        )));
    }

    // Determine which providers to sync
    let providers = match provider {
        Some(p) => vec![p.to_string()],
        None => vec!["github".to_string(), "buildkite".to_string()],
    };

    let mut outputs = Vec::new();
    let mut errors: Vec<(String, cuenv_core::Error)> = Vec::new();

    for prov in &providers {
        let result = match prov.as_str() {
            "github" => execute_sync_github(&repo_root, dry_run, check, &target_projects).await,
            "buildkite" => execute_sync_buildkite(&repo_root, dry_run, check),
            _ => Err(cuenv_core::Error::configuration(format!(
                "Unsupported CI provider: {prov}. Supported: github, buildkite"
            ))),
        };

        match result {
            Ok(output) if !output.is_empty() => outputs.push(output),
            Ok(_) => {} // Skip empty output (no config for this provider)
            Err(e) => {
                if provider.is_some() {
                    return Err(e);
                }
                tracing::debug!("Skipping {prov}: {e}");
                errors.push((prov.clone(), e));
            }
        }
    }

    if outputs.is_empty() {
        if errors.is_empty() {
            Ok("No CI configuration found.".to_string())
        } else {
            // CI config exists but all providers had errors
            let error_summary: Vec<String> = errors
                .iter()
                .map(|(prov, e)| format!("{prov}: {e}"))
                .collect();
            Ok(format!(
                "CI sync failed for all providers:\n{}",
                error_summary.join("\n")
            ))
        }
    } else {
        Ok(outputs.join("\n"))
    }
}

/// Execute workspace-wide CI sync.
///
/// Syncs CI workflow files for all projects with CI configuration.
///
/// # Errors
///
/// Returns an error if module evaluation or workflow generation fails.
#[instrument(name = "sync_ci_workspace", skip_all)]
pub async fn execute_sync_ci_workspace(
    _package: &str,
    dry_run: bool,
    check: bool,
    provider: Option<&str>,
    executor: &CommandExecutor,
) -> Result<String> {
    // Get cached module from executor and discover projects before async work
    // (ModuleGuard contains MutexGuard which is not Send, must be dropped before await)
    let projects = {
        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;
        let module = executor.get_module(&cwd)?;
        ProjectInfo::collect_from_module(&module)?
    };

    if projects.is_empty() {
        return Ok("No projects with CI configuration found.".to_string());
    }

    let mut outputs = Vec::new();

    for project in &projects {
        // Use absolute path - relative_path is relative to module root, not CWD
        let project_path_str = project.project_path.to_string_lossy();

        let result = execute_sync_ci(
            &project_path_str,
            "cuenv",
            dry_run,
            check,
            provider,
            executor,
        )
        .await;

        match result {
            Ok(output) if !output.is_empty() => {
                outputs.push(format!("[{}]\n{}", project.config.name, output));
            }
            Ok(_) => {}
            Err(e) => {
                outputs.push(format!("[{}] Error: {}", project.config.name, e));
            }
        }
    }

    if outputs.is_empty() {
        Ok("No CI workflows to sync.".to_string())
    } else {
        Ok(outputs.join("\n\n"))
    }
}

/// Sync GitHub Actions workflow files from CUE configuration.
#[allow(clippy::too_many_lines)]
#[instrument(name = "sync_github", skip_all)]
async fn execute_sync_github(
    repo_root: &Path,
    dry_run: bool,
    check: bool,
    projects: &[ProjectInfo],
) -> Result<String> {
    if projects.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "No cuenv projects found. Ensure env.cue files declare 'package cuenv'",
        ));
    }

    // Generate workflows per-project, per-pipeline
    // Each project with CI config gets its own workflow files
    let mut all_workflows: Vec<(String, String)> = Vec::new();
    for project in projects {
        let Some(ci) = &project.config.ci else {
            continue;
        };
        for (pipeline_name, pipeline) in &ci.pipelines {
            let workflows = generate_github_workflow_for_project(project, pipeline_name, pipeline)?;
            all_workflows.extend(workflows);
        }
    }

    if all_workflows.is_empty() {
        return Ok(String::new());
    }

    let workflows_dir = repo_root.join(".github/workflows");
    let mut output_lines = Vec::new();

    // Check mode: compare generated content with existing files
    if check {
        let mut out_of_sync = Vec::new();
        for (filename, content) in &all_workflows {
            let path = workflows_dir.join(filename);
            if path.exists() {
                let existing =
                    std::fs::read_to_string(&path).map_err(|e| cuenv_core::Error::Io {
                        source: e,
                        path: Some(path.clone().into_boxed_path()),
                        operation: "read workflow file".to_string(),
                    })?;
                if existing != *content {
                    out_of_sync.push(filename.clone());
                }
            } else {
                out_of_sync.push(format!("{filename} (missing)"));
            }
        }
        if !out_of_sync.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "GitHub workflows out of sync: {}. Run 'cuenv sync ci' to update.",
                out_of_sync.join(", ")
            )));
        }
        return Ok(format!(
            "GitHub: {} workflow(s) in sync",
            all_workflows.len()
        ));
    }

    // Dry-run or normal mode
    for (filename, content) in &all_workflows {
        let workflow_path = workflows_dir.join(filename);
        let exists = workflow_path.exists();

        // Check if content matches (skip if unchanged)
        if exists && !dry_run {
            let existing = std::fs::read_to_string(&workflow_path).unwrap_or_default();
            if existing == *content {
                output_lines.push(format!("GitHub: {filename} (unchanged)"));
                continue;
            }
        }

        if dry_run {
            if exists {
                output_lines.push(format!("GitHub: Would update {filename}"));
            } else {
                output_lines.push(format!("GitHub: Would create {filename}"));
            }
        } else {
            // Create directory if needed
            if !workflows_dir.exists() {
                std::fs::create_dir_all(&workflows_dir).map_err(|e| cuenv_core::Error::Io {
                    source: e,
                    path: Some(workflows_dir.clone().into_boxed_path()),
                    operation: "create directory".to_string(),
                })?;
            }

            std::fs::write(&workflow_path, content).map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(workflow_path.clone().into_boxed_path()),
                operation: "write workflow file".to_string(),
            })?;

            if exists {
                output_lines.push(format!("GitHub: Updated {filename}"));
            } else {
                output_lines.push(format!("GitHub: Created {filename}"));
            }
        }
    }

    Ok(output_lines.join("\n"))
}

/// Collected pipeline context from project discovery.
struct PipelineContext {
    is_release: bool,
    /// Pipeline generation mode (thin vs expanded)
    mode: cuenv_core::ci::PipelineMode,
    github_config: cuenv_github::config::GitHubConfig,
    trigger: cuenv_ci::ir::TriggerCondition,
    project_name: Option<String>,
    /// Relative path to project directory (for working-directory in monorepos)
    project_path: Option<String>,
    environment: Option<String>,
    runtimes: Vec<cuenv_ci::ir::Runtime>,
    /// All tasks including phase tasks (phase tasks have phase field set)
    tasks: Vec<cuenv_ci::ir::Task>,
    /// Original pipeline tasks (with matrix/artifacts/params info)
    pipeline_tasks: Vec<cuenv_core::ci::PipelineTask>,
}

impl PipelineContext {
    /// Build an IntermediateRepresentation from this context.
    fn to_ir(&self, pipeline_name: &str) -> cuenv_ci::ir::IntermediateRepresentation {
        cuenv_ci::ir::IntermediateRepresentation {
            version: "1.5".to_string(),
            pipeline: cuenv_ci::ir::PipelineMetadata {
                name: pipeline_name.to_string(),
                mode: self.mode,
                environment: self.environment.clone(),
                requires_onepassword: false,
                project_name: self.project_name.clone(),
                trigger: Some(self.trigger.clone()),
                pipeline_tasks: self
                    .pipeline_tasks
                    .iter()
                    .map(|t| t.task_name().to_string())
                    .collect(),
                pipeline_task_defs: self.pipeline_tasks.clone(),
            },
            runtimes: self.runtimes.clone(),
            tasks: self.tasks.clone(),
        }
    }

    /// Get regular (non-phase) tasks from this context.
    fn regular_tasks(&self) -> Vec<&cuenv_ci::ir::Task> {
        self.tasks.iter().filter(|t| t.phase.is_none()).collect()
    }
}

/// Check if any pipeline tasks have matrix configurations that require expansion.
///
/// Returns true only for tasks with actual matrix dimensions (non-empty matrix map).
/// Aggregation tasks (empty matrix with artifacts) return false.
fn has_matrix_tasks(pipeline_tasks: &[cuenv_core::ci::PipelineTask]) -> bool {
    pipeline_tasks
        .iter()
        .any(cuenv_core::ci::PipelineTask::has_matrix_dimensions)
}

/// Generate GitHub workflow files for a single project and pipeline.
fn generate_github_workflow_for_project(
    project: &ProjectInfo,
    pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
) -> Result<Vec<(String, String)>> {
    use cuenv_core::ci::PipelineMode;

    let ctx = build_project_pipeline_context(project, pipeline_name, pipeline)?;

    // Dispatch based on pipeline mode
    // Note: Matrix tasks ALWAYS require multi-job workflow regardless of mode,
    // since they need to run on different runners for each matrix dimension.
    match ctx.mode {
        PipelineMode::Thin => {
            // Thin mode with matrix tasks still needs multi-job workflow
            if has_matrix_tasks(&ctx.pipeline_tasks) {
                emit_matrix_workflow(pipeline_name, &ctx)
            } else {
                // Pure thin mode: single job with cuenv ci orchestration
                emit_thin_workflow(pipeline_name, &ctx)
            }
        }
        PipelineMode::Expanded => {
            // Expanded mode: all tasks as individual jobs with dependencies
            if has_matrix_tasks(&ctx.pipeline_tasks) {
                emit_matrix_workflow(pipeline_name, &ctx)
            } else if ctx.is_release {
                emit_release_workflow(pipeline_name, &ctx)
            } else if ctx.tasks.is_empty() {
                Ok(Vec::new())
            } else {
                emit_standard_workflow(pipeline_name, &ctx)
            }
        }
    }
}

/// Build pipeline context for a single project and pipeline.
fn build_project_pipeline_context(
    project: &ProjectInfo,
    pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
) -> Result<PipelineContext> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};

    let ci = project
        .config
        .ci
        .as_ref()
        .ok_or_else(|| cuenv_core::Error::configuration("Project has no CI configuration"))?;

    // Detect release pipelines by checking if they have release event triggers
    let is_release = pipeline.when.as_ref().is_some_and(|w| w.release.is_some());

    // Compute project_path for compiler (None if root, i.e., empty relative_path)
    let project_path_for_compiler = if project.relative_path.as_os_str().is_empty() {
        None
    } else {
        Some(project.relative_path.to_string_lossy().to_string())
    };

    let options = CompilerOptions {
        pipeline_name: Some(pipeline_name.to_string()),
        pipeline: Some(pipeline.clone()),
        ci_mode: true,
        module_root: Some(project.module_root.clone()),
        project_path: project_path_for_compiler.clone(),
        ..Default::default()
    };
    let compiler = Compiler::with_options(project.config.clone(), options);
    let ir = compiler
        .compile()
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to compile project: {e}")))?;

    // Extract task names from pipeline tasks (which can be simple strings or matrix tasks)
    let pipeline_task_names: Vec<String> = pipeline
        .tasks
        .iter()
        .map(|t| t.task_name().to_string())
        .collect();

    // Get pipeline tasks (non-phase tasks)
    let filtered_tasks = cuenv_ci::pipeline::filter_tasks(&pipeline_task_names, ir.tasks.clone());

    // Combine phase tasks (bootstrap, setup, success, failure) with pipeline tasks
    let phase_tasks: Vec<cuenv_ci::ir::Task> =
        ir.tasks.into_iter().filter(|t| t.phase.is_some()).collect();
    let mut all_tasks = phase_tasks;
    all_tasks.extend(filtered_tasks);

    // Use the compiler-derived trigger which includes paths from task inputs
    let trigger = ir
        .pipeline
        .trigger
        .unwrap_or_else(|| build_github_trigger_condition(pipeline_name, pipeline, ci));

    Ok(PipelineContext {
        is_release,
        mode: pipeline.mode,
        github_config: ci.github_config_for_pipeline(pipeline_name),
        trigger,
        project_name: Some(project.config.name.clone()),
        project_path: project_path_for_compiler,
        environment: pipeline.environment.clone(),
        runtimes: ir.runtimes,
        tasks: all_tasks,
        pipeline_tasks: pipeline.tasks.clone(),
    })
}

/// Emit a release workflow using the `ReleaseWorkflowBuilder`.
fn emit_release_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::{GitHubActionsEmitter, ReleaseWorkflowBuilder};

    let ir = ctx.to_ir(pipeline_name);

    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();
    let workflow = ReleaseWorkflowBuilder::new(emitter).build(&ir);

    let workflow_name = match &ir.pipeline.project_name {
        Some(project) => format!("{project}-{}", ir.pipeline.name),
        None => ir.pipeline.name.clone(),
    };
    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Emit a thin mode workflow.
///
/// Thin mode generates a single-job workflow that delegates execution to cuenv:
/// 1. Bootstrap phase steps (from contributors)
/// 2. Setup phase steps (from contributors)
/// 3. Main execution: `cuenv ci --pipeline <name>`
/// 4. Success phase steps (if: success())
/// 5. Failure phase steps (if: failure())
fn emit_thin_workflow(pipeline_name: &str, ctx: &PipelineContext) -> Result<Vec<(String, String)>> {
    use cuenv_ci::ir::BuildStage;
    use cuenv_github::workflow::GitHubActionsEmitter;
    use cuenv_github::workflow::schema::{
        Concurrency, Environment, Job, PermissionLevel, Permissions, Step, Workflow,
    };
    use cuenv_github::workflow::stage_renderer::{GitHubStageRenderer, transform_secret_ref};
    use indexmap::IndexMap;

    let workflow_name = match &ctx.project_name {
        Some(project) => format!("{project}-{pipeline_name}"),
        None => pipeline_name.to_string(),
    };

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();
    let renderer = GitHubStageRenderer::new();

    // Build steps for the single job
    let mut steps = Vec::new();

    // Checkout step
    steps.push(Step::uses("actions/checkout@v4").with_name("Checkout"));

    // Bootstrap and setup phase steps (from contributors)
    let (phase_steps, secret_env) = GitHubActionsEmitter::render_phase_steps(&ir);
    steps.extend(phase_steps);

    // Main execution step: cuenv ci --pipeline <name>
    let cuenv_command = if let Some(ref project_path) = ctx.project_path {
        format!("cuenv ci --pipeline {pipeline_name} --path {project_path}")
    } else {
        format!("cuenv ci --pipeline {pipeline_name}")
    };

    let mut main_step = Step::run(&cuenv_command)
        .with_name(format!("Run pipeline: {pipeline_name}"))
        .with_env("GITHUB_TOKEN", "${{ secrets.GITHUB_TOKEN }}");

    if let Some(env) = &ctx.environment {
        main_step = main_step.with_env("CUENV_ENVIRONMENT", env.clone());
    }

    // Pass secret env vars from setup tasks to main step (e.g., OP_SERVICE_ACCOUNT_TOKEN)
    // Transform ${VAR} to ${{ secrets.VAR }} format for GitHub Actions
    for (key, value) in secret_env {
        main_step = main_step.with_env(key, transform_secret_ref(&value));
    }

    steps.push(main_step);

    // Success phase steps (from contributors)
    for task in ir.sorted_phase_tasks(BuildStage::Success) {
        let mut step = renderer.render_task(task);
        step.if_condition = Some("success()".to_string());
        steps.push(step);
    }

    // Failure phase steps (from contributors)
    for task in ir.sorted_phase_tasks(BuildStage::Failure) {
        let mut step = renderer.render_task(task);
        step.if_condition = Some("failure()".to_string());
        steps.push(step);
    }

    // Build the single job
    let job = Job {
        name: Some(workflow_name.clone()),
        runs_on: emitter.runner_as_runs_on(),
        needs: Vec::new(),
        if_condition: None,
        strategy: None,
        environment: ctx.environment.clone().map(Environment::Name),
        env: IndexMap::new(),
        concurrency: None,
        continue_on_error: None,
        timeout_minutes: None,
        steps,
    };

    let mut jobs = IndexMap::new();
    jobs.insert(sanitize_workflow_name(&workflow_name), job);

    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let workflow = Workflow {
        name: workflow_name,
        on: build_workflow_triggers(&ctx.trigger, &filename, &emitter),
        concurrency: Some(Concurrency {
            group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        }),
        permissions: Some(Permissions {
            contents: Some(PermissionLevel::Read),
            checks: Some(PermissionLevel::Write),
            pull_requests: Some(PermissionLevel::Write),
            ..Default::default()
        }),
        env: IndexMap::new(),
        jobs,
    };

    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Emit a standard workflow using the `GitHubActionsEmitter`.
///
/// Builds jobs directly using `build_simple_job` which supports `project_path`
/// for setting working-directory in monorepo workflows.
fn emit_standard_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_ci::ir::OutputType;
    use cuenv_github::workflow::GitHubActionsEmitter;
    use cuenv_github::workflow::schema::{Concurrency, PermissionLevel, Permissions, Workflow};
    use indexmap::IndexMap;

    let workflow_name = match &ctx.project_name {
        Some(project) => format!("{project}-{pipeline_name}"),
        None => pipeline_name.to_string(),
    };

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();

    // Build jobs using build_simple_job (which supports project_path for working-directory)
    // Only iterate over regular tasks (non-phase tasks) - phase tasks are handled internally
    let mut jobs = IndexMap::new();
    for task in ctx.regular_tasks() {
        let mut job = emitter.build_simple_job(
            task,
            &ir,
            ctx.environment.as_ref(),
            ctx.project_path.as_deref(),
        );
        job.needs = task
            .depends_on
            .iter()
            .map(|d| d.replace(['.', ' '], "-"))
            .collect();
        jobs.insert(task.id.replace(['.', ' '], "-"), job);
    }

    // Build permissions based on task requirements
    let has_deployments = ctx.tasks.iter().any(|t| t.deployment);
    let has_outputs = ctx.tasks.iter().any(|t| {
        t.outputs
            .iter()
            .any(|o| o.output_type == OutputType::Orchestrator)
    });

    let base_permissions = Permissions {
        contents: Some(if has_deployments {
            PermissionLevel::Write
        } else {
            PermissionLevel::Read
        }),
        checks: Some(PermissionLevel::Write),
        pull_requests: Some(PermissionLevel::Write),
        packages: if has_outputs {
            Some(PermissionLevel::Write)
        } else {
            None
        },
        ..Default::default()
    };

    // Apply configured permissions from the manifest
    let permissions = emitter.apply_configured_permissions(base_permissions);

    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let workflow = Workflow {
        name: workflow_name.clone(),
        on: build_workflow_triggers(&ctx.trigger, &filename, &emitter),
        concurrency: Some(Concurrency {
            group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        }),
        permissions: Some(permissions),
        env: IndexMap::new(),
        jobs,
    };
    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Build jobs from expanded pipeline tasks, tracking artifact sources.
///
/// Uses `GitHubActionsEmitter` methods to build jobs, converting `PipelineTask`
/// info to IR `Task` fields as needed.
fn build_pipeline_jobs(
    expanded_tasks: &[cuenv_core::ci::PipelineTask],
    ctx: &PipelineContext,
    ir: &cuenv_ci::ir::IntermediateRepresentation,
    emitter: &cuenv_github::workflow::GitHubActionsEmitter,
) -> indexmap::IndexMap<String, cuenv_github::workflow::schema::Job> {
    use indexmap::IndexMap;

    let mut jobs = IndexMap::new();
    let mut artifact_source_jobs: HashSet<String> = HashSet::new();
    let mut processed_task_names: HashSet<String> = HashSet::new();

    for pipeline_task in expanded_tasks {
        let task_name = pipeline_task.task_name();
        processed_task_names.insert(task_name.to_string());
        let job_id = task_name.replace(['.', ' '], "-");

        match pipeline_task {
            cuenv_core::ci::PipelineTask::Simple(_) => {
                if let Some(ir_task) = ctx.tasks.iter().find(|t| t.id == task_name) {
                    // Use emitter method directly
                    let mut job = emitter.build_simple_job(
                        ir_task,
                        ir,
                        ctx.environment.as_ref(),
                        ctx.project_path.as_deref(),
                    );
                    job.needs = ir_task
                        .depends_on
                        .iter()
                        .map(|dep| dep.replace(['.', ' '], "-"))
                        .collect();
                    jobs.insert(job_id, job);
                }
            }
            cuenv_core::ci::PipelineTask::Matrix(matrix_task) => {
                if matrix_task.matrix.is_empty() {
                    // Artifact aggregation task: create a synthetic IR Task with artifact_downloads
                    let ir_task = ctx.tasks.iter().find(|t| t.id == task_name);
                    let mut seen: HashSet<String> = artifact_source_jobs.clone();
                    let mut combined_needs: Vec<String> =
                        artifact_source_jobs.iter().cloned().collect();

                    if let Some(ir_task) = ir_task {
                        for dep in &ir_task.depends_on {
                            let dep_job_id = dep.replace(['.', ' '], "-");
                            if seen.insert(dep_job_id.clone()) {
                                combined_needs.push(dep_job_id);
                            }
                        }
                    }
                    // Sort for deterministic output
                    combined_needs.sort();

                    // Create synthetic IR Task with artifact_downloads and params
                    let synthetic_task = create_synthetic_aggregation_task(task_name, matrix_task);
                    let job = emitter.build_artifact_aggregation_job(
                        &synthetic_task,
                        ir,
                        ctx.environment.as_ref(),
                        &combined_needs,
                        ctx.project_path.as_deref(),
                    );
                    jobs.insert(job_id, job);
                } else {
                    // Matrix expansion task: create a synthetic IR Task with matrix config
                    // Look up the actual task to get its outputs for artifact upload paths
                    let ir_task = ctx.tasks.iter().find(|t| t.id == task_name);
                    let outputs = ir_task.map(|t| t.outputs.clone()).unwrap_or_default();
                    let synthetic_task =
                        create_synthetic_matrix_task(task_name, matrix_task, outputs);
                    let arch_runners = ctx
                        .github_config
                        .runners
                        .as_ref()
                        .and_then(|r| r.arch.clone());

                    let expanded_jobs = emitter.build_matrix_jobs(
                        &synthetic_task,
                        ir,
                        ctx.environment.as_ref(),
                        arch_runners.as_ref(),
                        &[],
                        ctx.project_path.as_deref(),
                    );

                    for (id, job) in expanded_jobs {
                        artifact_source_jobs.insert(id.clone());
                        jobs.insert(id, job);
                    }
                }
            }
        }
    }

    // Add transitive dependencies not in pipeline tasks
    // NOTE: We ONLY add non-phase tasks here. Phase tasks (bootstrap, setup, success, failure)
    // are rendered as STEPS within jobs via render_phase_steps(), NOT as separate jobs.
    for ir_task in &ctx.tasks {
        // Skip phase tasks - they're rendered as STEPS within jobs, not separate jobs
        if ir_task.phase.is_some() {
            continue;
        }

        // Skip if this task was explicitly in the pipeline (including as matrix task)
        if processed_task_names.contains(&ir_task.id) {
            continue;
        }

        let job_id = ir_task.id.replace(['.', ' '], "-");
        if jobs.contains_key(&job_id) {
            continue;
        }

        // Use emitter method directly
        let mut job = emitter.build_simple_job(
            ir_task,
            ir,
            ctx.environment.as_ref(),
            ctx.project_path.as_deref(),
        );
        job.needs = ir_task
            .depends_on
            .iter()
            .map(|dep| dep.replace(['.', ' '], "-"))
            .collect();
        jobs.insert(job_id, job);
    }

    jobs
}

/// Create a synthetic IR Task for artifact aggregation from a `MatrixTask`.
///
/// Converts `MatrixTask.artifacts` to IR `Task.artifact_downloads` and
/// `MatrixTask.params` to IR `Task.params`.
fn create_synthetic_aggregation_task(
    task_name: &str,
    matrix_task: &cuenv_core::ci::MatrixTask,
) -> cuenv_ci::ir::Task {
    use cuenv_ci::ir::{ArtifactDownload, CachePolicy, Task};

    let artifact_downloads = matrix_task
        .artifacts
        .as_ref()
        .map(|artifacts| {
            artifacts
                .iter()
                .map(|a| ArtifactDownload {
                    name: a.from.replace('.', "-"),
                    path: a.to.clone(),
                    filter: String::new(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Convert HashMap to BTreeMap for IR compatibility
    let params: BTreeMap<String, String> = matrix_task
        .params
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    Task {
        id: task_name.to_string(),
        runtime: None,
        command: vec![],
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs: vec![],
        depends_on: vec![],
        cache_policy: CachePolicy::Normal,
        deployment: false,
        manual_approval: false,
        matrix: None,
        artifact_downloads,
        params,
        // Phase task fields (not applicable for sync tasks)
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

/// Create a synthetic IR Task for matrix expansion from a `MatrixTask`.
///
/// Converts `MatrixTask.matrix` to IR `Task.matrix`.
fn create_synthetic_matrix_task(
    task_name: &str,
    matrix_task: &cuenv_core::ci::MatrixTask,
    outputs: Vec<cuenv_ci::ir::OutputDeclaration>,
) -> cuenv_ci::ir::Task {
    use cuenv_ci::ir::{CachePolicy, MatrixConfig, Task};

    // Convert HashMap to BTreeMap for IR compatibility
    // Sort dimension values for deterministic output
    let dimensions: BTreeMap<String, Vec<String>> = matrix_task
        .matrix
        .iter()
        .map(|(k, v)| {
            let mut sorted_values = v.clone();
            sorted_values.sort();
            (k.clone(), sorted_values)
        })
        .collect();

    let matrix = MatrixConfig {
        dimensions,
        exclude: vec![],
        include: vec![],
        max_parallel: 0,
        fail_fast: true,
    };

    Task {
        id: task_name.to_string(),
        runtime: None,
        command: vec![],
        shell: false,
        env: BTreeMap::new(),
        secrets: BTreeMap::new(),
        resources: None,
        concurrency_group: None,
        inputs: vec![],
        outputs,
        depends_on: vec![],
        cache_policy: CachePolicy::Normal,
        deployment: false,
        manual_approval: false,
        matrix: Some(matrix),
        artifact_downloads: vec![],
        params: BTreeMap::new(),
        // Phase task fields (not applicable for matrix tasks)
        phase: None,
        label: None,
        priority: None,
        contributor: None,
        condition: None,
        provider_hints: None,
    }
}

/// Emit a workflow with matrix expansion for tasks that have matrix configurations.
fn emit_matrix_workflow(
    pipeline_name: &str,
    ctx: &PipelineContext,
) -> Result<Vec<(String, String)>> {
    use cuenv_github::workflow::GitHubActionsEmitter;
    use cuenv_github::workflow::schema::{Concurrency, PermissionLevel, Permissions, Workflow};

    let workflow_name = match &ctx.project_name {
        Some(project) => format!("{project}-{pipeline_name}"),
        None => pipeline_name.to_string(),
    };

    let ir = ctx.to_ir(pipeline_name);
    let emitter = GitHubActionsEmitter::from_config(&ctx.github_config).with_nix();

    let explicit_task_names: HashSet<String> = ctx
        .pipeline_tasks
        .iter()
        .map(|pt| pt.task_name().to_string())
        .collect();

    let expanded_tasks = cuenv_ci::pipeline::expand_task_groups(
        &ctx.pipeline_tasks,
        &ctx.tasks,
        &explicit_task_names,
    );

    let jobs = build_pipeline_jobs(&expanded_tasks, ctx, &ir, &emitter);

    let filename = format!("{}.yml", sanitize_workflow_name(&workflow_name));

    let workflow = Workflow {
        name: workflow_name.clone(),
        on: build_workflow_triggers(&ctx.trigger, &filename, &emitter),
        concurrency: Some(Concurrency {
            group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        }),
        permissions: Some(Permissions {
            contents: Some(PermissionLevel::Write),
            id_token: Some(PermissionLevel::Write),
            ..Default::default()
        }),
        env: indexmap::IndexMap::new(),
        jobs,
    };
    let yaml = workflow.to_yaml().map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize workflow: {e}"))
    })?;

    Ok(vec![(filename, yaml)])
}

/// Build workflow triggers from the trigger condition.
///
/// Adds the workflow file itself to the paths if paths are non-empty,
/// ensuring workflows trigger when their own definition changes.
fn build_workflow_triggers(
    trigger: &cuenv_ci::ir::TriggerCondition,
    workflow_filename: &str,
    _emitter: &cuenv_github::workflow::GitHubActionsEmitter,
) -> cuenv_github::workflow::schema::WorkflowTriggers {
    use cuenv_github::workflow::schema::{
        PullRequestTrigger, PushTrigger, ReleaseTrigger, ScheduleTrigger, WorkflowDispatchTrigger,
        WorkflowInput, WorkflowTriggers,
    };

    // Add workflow file to paths so changes to the workflow itself trigger a run
    let paths = if trigger.paths.is_empty() {
        Vec::new()
    } else {
        let workflow_path = format!(".github/workflows/{workflow_filename}");
        let mut paths = trigger.paths.clone();
        if !paths.contains(&workflow_path) {
            paths.push(workflow_path);
            paths.sort();
        }
        paths
    };

    let push = if trigger.branches.is_empty() {
        None
    } else {
        Some(PushTrigger {
            branches: trigger.branches.clone(),
            paths: paths.clone(),
            paths_ignore: trigger.paths_ignore.clone(),
            ..Default::default()
        })
    };

    let pull_request = if trigger.pull_request == Some(true) {
        Some(PullRequestTrigger {
            branches: trigger.branches.clone(),
            paths,
            paths_ignore: trigger.paths_ignore.clone(),
            ..Default::default()
        })
    } else {
        None
    };

    let release = if trigger.release.is_empty() {
        None
    } else {
        Some(ReleaseTrigger {
            types: trigger.release.clone(),
        })
    };

    let schedule = if trigger.scheduled.is_empty() {
        None
    } else {
        Some(
            trigger
                .scheduled
                .iter()
                .map(|cron| ScheduleTrigger { cron: cron.clone() })
                .collect(),
        )
    };

    let workflow_dispatch = trigger.manual.as_ref().and_then(|m| {
        if !m.enabled && m.inputs.is_empty() {
            return None;
        }
        Some(WorkflowDispatchTrigger {
            inputs: m
                .inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowInput {
                            description: v.description.clone(),
                            required: Some(v.required),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: if v.options.is_empty() {
                                None
                            } else {
                                Some(v.options.clone())
                            },
                        },
                    )
                })
                .collect(),
        })
    });

    WorkflowTriggers {
        push,
        pull_request,
        release,
        workflow_dispatch,
        schedule,
    }
}

/// Sanitize a workflow name for use as a filename.
fn sanitize_workflow_name(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Build GitHub Actions trigger condition from pipeline config.
fn build_github_trigger_condition(
    pipeline_name: &str,
    pipeline: &cuenv_core::ci::Pipeline,
    ci_config: &cuenv_core::ci::CI,
) -> cuenv_ci::ir::TriggerCondition {
    use cuenv_ci::ir::{ManualTriggerConfig, TriggerCondition, WorkflowDispatchInputDef};
    use cuenv_core::ci::ManualTrigger;

    let when = pipeline.when.as_ref();

    let branches = when
        .and_then(|w| w.branch.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    let pull_request = when.and_then(|w| w.pull_request);

    let scheduled = when
        .and_then(|w| w.scheduled.as_ref())
        .map(cuenv_core::ci::StringOrVec::to_vec)
        .unwrap_or_default();

    let release = when.and_then(|w| w.release.clone()).unwrap_or_default();

    let manual = when.and_then(|w| w.manual.as_ref()).map(|m| match m {
        ManualTrigger::Enabled(enabled) => ManualTriggerConfig {
            enabled: *enabled,
            inputs: BTreeMap::new(),
        },
        ManualTrigger::WithInputs(inputs) => ManualTriggerConfig {
            enabled: true,
            inputs: inputs
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        WorkflowDispatchInputDef {
                            description: v.description.clone(),
                            required: v.required.unwrap_or(false),
                            default: v.default.clone(),
                            input_type: v.input_type.clone(),
                            options: v.options.clone().unwrap_or_default(),
                        },
                    )
                })
                .collect(),
        },
    });

    let paths_ignore = ci_config
        .github_config_for_pipeline(pipeline_name)
        .paths_ignore
        .unwrap_or_default();

    TriggerCondition {
        branches,
        pull_request,
        scheduled,
        release,
        manual,
        paths: Vec::new(),
        paths_ignore,
    }
}

/// Sync Buildkite bootstrap pipeline file.
#[instrument(name = "sync_buildkite", skip_all)]
fn execute_sync_buildkite(repo_root: &Path, dry_run: bool, check: bool) -> Result<String> {
    // Note: Using --dynamic instead of --format for the new CLI
    let pipeline_content = r#"# Buildkite bootstrap pipeline for cuenv
# This installs Nix, builds cuenv, then generates a dynamic pipeline
steps:
  - label: ":nix: Install Nix"
    key: install-nix
    command: |
      curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none
      . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
      nix --version

  - label: ":package: Build cuenv"
    key: build-cuenv
    depends_on: install-nix
    command: |
      . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
      nix build .#cuenv --accept-flake-config
      echo "$(pwd)/result/bin" >> "$BUILDKITE_ENV_FILE"

  - label: ":pipeline: Generate Pipeline"
    depends_on: build-cuenv
    command: cuenv ci --dynamic buildkite | buildkite-agent pipeline upload
"#;

    let buildkite_dir = repo_root.join(".buildkite");
    let pipeline_path = buildkite_dir.join("pipeline.yml");

    // Check mode
    if check {
        if pipeline_path.exists() {
            let existing = std::fs::read_to_string(&pipeline_path).unwrap_or_default();
            if existing == pipeline_content {
                return Ok("Buildkite: pipeline.yml in sync".to_string());
            }
            return Err(cuenv_core::Error::configuration(
                "Buildkite pipeline.yml out of sync. Run 'cuenv sync ci --provider buildkite' to update.",
            ));
        }
        return Err(cuenv_core::Error::configuration(
            "Buildkite pipeline.yml missing. Run 'cuenv sync ci --provider buildkite' to create.",
        ));
    }

    let exists = pipeline_path.exists();

    // Check if file exists and matches (skip if unchanged)
    if exists && !dry_run {
        let existing = std::fs::read_to_string(&pipeline_path).unwrap_or_default();
        if existing == pipeline_content {
            return Ok("Buildkite: pipeline.yml (unchanged)".to_string());
        }
    }

    // Dry-run mode
    if dry_run {
        if exists {
            return Ok("Buildkite: Would update pipeline.yml".to_string());
        }
        return Ok("Buildkite: Would create pipeline.yml".to_string());
    }

    // Create directory if needed
    if !buildkite_dir.exists() {
        std::fs::create_dir_all(&buildkite_dir).map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(buildkite_dir.clone().into_boxed_path()),
            operation: "create directory".to_string(),
        })?;
    }

    // Write file
    std::fs::write(&pipeline_path, pipeline_content).map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(pipeline_path.clone().into_boxed_path()),
        operation: "write pipeline file".to_string(),
    })?;

    if exists {
        Ok("Buildkite: Updated pipeline.yml".to_string())
    } else {
        Ok("Buildkite: Created pipeline.yml".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::ci::{MatrixTask, PipelineTask};
    use std::collections::BTreeMap;

    #[test]
    fn test_has_matrix_tasks_empty() {
        let tasks: Vec<PipelineTask> = vec![];
        assert!(!has_matrix_tasks(&tasks));
    }

    #[test]
    fn test_has_matrix_tasks_simple_only() {
        let tasks = vec![
            PipelineTask::Simple("build".to_string()),
            PipelineTask::Simple("test".to_string()),
        ];
        assert!(!has_matrix_tasks(&tasks));
    }

    #[test]
    fn test_has_matrix_tasks_with_matrix() {
        let mut matrix = BTreeMap::new();
        matrix.insert(
            "arch".to_string(),
            vec!["linux-x64".to_string(), "darwin-arm64".to_string()],
        );

        let tasks = vec![PipelineTask::Matrix(MatrixTask {
            task: "cargo.build".to_string(),
            matrix,
            artifacts: None,
            params: None,
        })];
        assert!(has_matrix_tasks(&tasks));
    }

    #[test]
    fn test_has_matrix_tasks_aggregation_only() {
        // Aggregation task has empty matrix but artifacts
        let tasks = vec![PipelineTask::Matrix(MatrixTask {
            task: "publish".to_string(),
            matrix: BTreeMap::new(),
            artifacts: Some(vec![]),
            params: None,
        })];
        // Aggregation tasks are NOT matrix tasks (they don't have matrix dimensions)
        assert!(!has_matrix_tasks(&tasks));
    }

    #[test]
    fn test_has_matrix_tasks_mixed() {
        let mut matrix = BTreeMap::new();
        matrix.insert("arch".to_string(), vec!["linux-x64".to_string()]);

        let tasks = vec![
            PipelineTask::Simple("check".to_string()),
            PipelineTask::Matrix(MatrixTask {
                task: "build".to_string(),
                matrix,
                artifacts: None,
                params: None,
            }),
            PipelineTask::Simple("deploy".to_string()),
        ];
        assert!(has_matrix_tasks(&tasks));
    }
}
