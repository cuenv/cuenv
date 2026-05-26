//! Sync function implementations.
//!
//! Supports generating:
//! - Project files from CUE codegen templates
//! - CI pipelines from CUE configuration
//!
//! Note: Ignore files and CODEOWNERS are now handled via .rules.cue files.
//! Use `cuenv sync rules` for those.

mod codegen;
mod github;

use super::super::CommandExecutor;
use cuenv_core::DryRun;
use cuenv_core::manifest::Project;
use cuenv_core::{ModuleEvaluation, Result};
use std::path::{Path, PathBuf};
use tracing::instrument;

pub use codegen::{CodegenSyncOptions, CodegenSyncRequest, execute_sync_codegen};
use github::{GithubSyncRequest, execute_sync_github};

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

/// Options controlling CI sync behavior.
#[derive(Clone, Copy, Debug)]
pub struct CiSyncOptions<'a> {
    /// Show what would be generated without writing files.
    pub dry_run: DryRun,
    /// Check if files are in sync without making changes.
    pub check: bool,
    /// Optional provider override.
    pub provider: Option<&'a str>,
}

/// Request for syncing CI for a single path.
#[derive(Debug)]
pub struct CiSyncRequest<'a> {
    /// Path to the CUE module or project directory.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Sync options.
    pub options: CiSyncOptions<'a>,
}

/// Request for syncing CI across the workspace.
#[derive(Debug)]
pub struct CiWorkspaceSyncRequest<'a> {
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Sync options.
    pub options: CiSyncOptions<'a>,
}

struct BuildkiteSyncRequest<'a> {
    repo_root: &'a Path,
    options: CiSyncOptions<'a>,
}
// ============================================================================
// CI Workflow Sync
// ============================================================================

/// Known CI providers supported by cuenv sync.
const KNOWN_PROVIDERS: &[&str] = &["github", "buildkite", "gitlab"];

/// Validate provider names against known providers.
///
/// Returns a tuple of (valid_providers, warnings).
/// Unknown providers generate warnings but don't prevent sync from proceeding
/// with the valid ones.
fn validate_providers(providers: &[String]) -> (Vec<String>, Vec<String>) {
    let mut valid = Vec::new();
    let mut warnings = Vec::new();

    for p in providers {
        if KNOWN_PROVIDERS.contains(&p.as_str()) {
            valid.push(p.clone());
        } else {
            warnings.push(format!(
                "Unknown CI provider '{}'. Known providers: {}",
                p,
                KNOWN_PROVIDERS.join(", ")
            ));
        }
    }

    (valid, warnings)
}

/// Execute the sync ci command for a single project.
///
/// Syncs CI workflow files (GitHub Actions, Buildkite) based on CUE configuration.
///
/// # Errors
///
/// Returns an error if project discovery fails or workflow generation fails.
#[instrument(name = "sync_ci", skip_all)]
pub async fn execute_sync_ci(
    request: CiSyncRequest<'_>,
    executor: &CommandExecutor,
) -> Result<String> {
    tracing::info!("Starting sync ci command");

    let dir_path = Path::new(request.path);

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
        // Fallback: when invoked at the module root and no specific project
        // matches, do not error out. In CI we often run `cuenv sync ci` from
        // the repo root just to ensure workflows are in sync; returning a
        // benign message avoids a hard failure.
        if repo_root == target_path {
            return Ok("No CI configuration found.".to_string());
        }
        return Err(cuenv_core::Error::configuration(format!(
            "No cuenv project found at path: {}. Run 'cuenv info' to inspect project layout or use 'cuenv sync -A' to sync all projects.",
            dir_path.display()
        )));
    }

    // Determine which providers to sync (CLI flag takes precedence)
    let providers: Vec<String> = if let Some(p) = request.options.provider {
        vec![p.to_string()]
    } else {
        // Use configured providers from CUE
        let ci_config = target_projects.first().and_then(|p| p.config.ci.as_ref());

        match ci_config {
            Some(ci) if !ci.providers.is_empty() => ci.providers.clone(),
            _ => {
                // No providers configured = emit nothing (explicit configuration required)
                return Ok(
                    "No CI providers configured. Add 'providers: [\"github\"]' to your ci config."
                        .to_string(),
                );
            }
        }
    };

    // Validate providers
    let (valid_providers, warnings) = validate_providers(&providers);
    for warning in &warnings {
        tracing::warn!("{}", warning);
    }

    if valid_providers.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "No valid CI providers specified. Known providers: {}",
            KNOWN_PROVIDERS.join(", ")
        )));
    }

    let mut outputs = Vec::new();
    let mut errors: Vec<(String, cuenv_core::Error)> = Vec::new();

    for prov in &valid_providers {
        let result = match prov.as_str() {
            "github" => {
                let github_request = GithubSyncRequest {
                    repo_root: &repo_root,
                    options: request.options,
                    projects: &target_projects,
                };
                execute_sync_github(github_request).await
            }
            "buildkite" => {
                let buildkite_request = BuildkiteSyncRequest {
                    repo_root: &repo_root,
                    options: request.options,
                };
                execute_sync_buildkite(&buildkite_request)
            }
            "gitlab" => {
                tracing::debug!("GitLab CI sync not yet implemented");
                continue;
            }
            _ => Err(cuenv_core::Error::configuration(format!(
                "Unsupported CI provider: {prov}. Supported: {}",
                KNOWN_PROVIDERS.join(", ")
            ))),
        };

        match result {
            Ok(output) if !output.is_empty() => outputs.push(output),
            Ok(_) => {} // Skip empty output (no config for this provider)
            Err(e) => {
                if request.options.provider.is_some() {
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
    request: CiWorkspaceSyncRequest<'_>,
    executor: &CommandExecutor,
) -> Result<String> {
    // Get cached module from executor and discover projects before async work
    // (ModuleGuard contains MutexGuard which is not Send, must be dropped before await)
    let projects = {
        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;
        let module = executor.discover_all_modules(&cwd)?;
        ProjectInfo::collect_from_module(&module)?
    };

    if projects.is_empty() {
        return Ok("No projects with CI configuration found.".to_string());
    }

    let mut outputs = Vec::new();

    for project in &projects {
        // Use absolute path - relative_path is relative to module root, not CWD
        let project_path_str = project.project_path.to_string_lossy();

        let ci_request = CiSyncRequest {
            path: &project_path_str,
            package: request.package,
            options: request.options,
        };
        let result = execute_sync_ci(ci_request, executor).await;

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

/// Sync Buildkite bootstrap pipeline file.
#[instrument(name = "sync_buildkite", skip_all)]
fn execute_sync_buildkite(request: &BuildkiteSyncRequest<'_>) -> Result<String> {
    let BuildkiteSyncRequest { repo_root, options } = *request;
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
    if options.check {
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
    if exists && !options.dry_run.is_dry_run() {
        let existing = std::fs::read_to_string(&pipeline_path).unwrap_or_default();
        if existing == pipeline_content {
            return Ok("Buildkite: pipeline.yml (unchanged)".to_string());
        }
    }

    // Dry-run mode
    if options.dry_run.is_dry_run() {
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
#[path = "functions_tests.rs"]
mod tests;
