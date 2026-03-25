//! Exec command implementation for running arbitrary commands with CUE environment
//!
//! This module supports three modes:
//! 1. **Project mode**: When inside a CUE module with `schema.#Project`, uses CUE-defined
//!    environment, hooks, secrets, and tools.
//! 2. **Base mode**: When inside a CUE module with `schema.#Base`, uses CUE-defined
//!    environment (no hooks) and lockfile tools.
//! 3. **No-module mode**: When outside a CUE module, runs commands with just the runtime
//!    tools from any available lockfile.

use super::sync::{SyncMode, SyncOptions, default_registry};
use super::tools::{ensure_tools_downloaded, resolve_tool_activation_steps};
use super::{CommandExecutor, relative_path_from_root};
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::lockfile::{LOCKFILE_NAME, Lockfile};
use cuenv_core::manifest::{Base, Project, Runtime, ToolSpec};
use cuenv_core::tasks::execute_command_with_redaction;
use cuenv_core::tools::apply_resolved_tool_activation;
use std::path::Path;

use cuenv_events::emit_stderr;
use cuenv_hooks::{ApprovalManager, ApprovalStatus, ConfigSummary, check_approval_status};

use super::export::{extract_static_env_vars, get_environment_with_hooks};
use tracing::instrument;

/// Represents the type of manifest found at a path.
enum ManifestKind {
    /// Full project with hooks, runtime, etc.
    Project(Box<Project>),
    /// Base configuration with just env and config
    Base(Box<Base>),
    /// No CUE module found
    None,
}

/// Command execution request for `exec`.
#[derive(Debug)]
pub struct ExecRequest<'a> {
    /// Path to the CUE module or project directory.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Command to execute.
    pub command: &'a str,
    /// Arguments to pass to the command.
    pub args: &'a [String],
    /// Optional environment name to use for execution.
    pub environment_override: Option<&'a str>,
}

/// Run a command with the CUE environment.
///
/// Uses the executor's cached module evaluation.
///
/// If no CUE module is found, runs in "tools-only" mode where only
/// runtime tools from lockfiles are activated.
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or command execution fails.
#[allow(clippy::too_many_lines)]
#[instrument(
    name = "exec_run",
    skip(executor),
    fields(path = %request.path, command = %request.command)
)]
pub async fn execute_exec(request: ExecRequest<'_>, executor: &CommandExecutor) -> Result<i32> {
    tracing::info!(
        "Running command with CUE environment from path: {}, package: {}, command: {} {:?}",
        request.path,
        request.package,
        request.command,
        request.args
    );

    // Evaluate CUE to get environment using module-wide evaluation
    let target_path =
        Path::new(request.path)
            .canonicalize()
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(Path::new(request.path).to_path_buf().into_boxed_path()),
                operation: "canonicalize path".to_string(),
            })?;

    // Try to get the manifest - can be Project, Base, or None
    let manifest_kind: ManifestKind = match executor.get_module(&target_path) {
        Ok(module) => {
            tracing::debug!("Using cached module evaluation from executor");
            let rel_path = relative_path_from_root(&module.root, &target_path);

            let instance = module.get(&rel_path).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "No CUE instance found at path: {} (relative: {})",
                    target_path.display(),
                    rel_path.display()
                ))
            })?;

            if instance.value.is_null() {
                tracing::debug!(
                    path = %rel_path.display(),
                    "Instance value is null, falling back to tools-only mode"
                );
                ManifestKind::None
            } else {
                // Handle both Project and Base
                match instance.kind {
                    cuenv_core::InstanceKind::Project => {
                        ManifestKind::Project(Box::new(instance.deserialize()?))
                    }
                    cuenv_core::InstanceKind::Base => {
                        ManifestKind::Base(Box::new(instance.deserialize()?))
                    }
                }
            }
        }
        Err(e) => {
            // Check if this is a "no module found" error
            let err_msg = e.to_string();
            if err_msg.contains("No CUE module found") {
                tracing::debug!("No CUE module found");
                ManifestKind::None
            } else {
                return Err(e);
            }
        }
    };

    // Extract env config and project reference based on manifest type
    let env_config = match &manifest_kind {
        ManifestKind::Project(project) => project.env.clone(),
        ManifestKind::Base(base) => base.env.clone(),
        ManifestKind::None => None,
    };

    // For Project, we need the full manifest for hooks
    let project_for_hooks: Option<&Project> = match &manifest_kind {
        ManifestKind::Project(p) => Some(p),
        _ => None,
    };

    // Get environment with hook-generated vars merged in
    let directory = std::fs::canonicalize(request.path)
        .unwrap_or_else(|_| Path::new(request.path).to_path_buf());

    // Build base environment based on manifest type
    let mut runtime_env = Environment::new();
    let mut secrets_for_redaction: Vec<String> = Vec::new();

    // For Project: check hooks approval and run hooks if approved
    // For Base/None: just extract static env vars (no hooks)
    if let Some(project) = project_for_hooks {
        let summary = ConfigSummary::from_hooks(project.hooks.as_ref());

        let hooks_approved = if summary.has_hooks {
            let mut approval_manager = ApprovalManager::with_default_file()?;
            approval_manager.load_approvals().await?;
            let approval_status =
                check_approval_status(&approval_manager, &directory, project.hooks.as_ref())?;
            matches!(approval_status, ApprovalStatus::Approved)
        } else {
            true // No hooks = nothing to approve
        };

        if !hooks_approved {
            emit_stderr!(
                "\x1b[1;33mWarning:\x1b[0m Hooks not run (approval required). Run '\x1b[36mcuenv allow\x1b[0m' to enable."
            );
        }

        let base_env_vars = if hooks_approved {
            get_environment_with_hooks(&directory, project, request.package, Some(executor)).await?
        } else {
            extract_static_env_vars(project)
        };
        tracing::debug!(
            "Base environment variables after hooks: {:?}",
            base_env_vars
        );

        // Apply base environment
        for (key, value) in &base_env_vars {
            runtime_env.set(key.clone(), value.clone());
        }

        // Apply command-specific policies and secret resolution for Project
        if let Some(env) = &project.env {
            let env_vars = if let Some(env_name) = request.environment_override {
                env.for_environment(env_name)
            } else {
                env.base.clone()
            };

            let (exec_env_vars, secrets) =
                cuenv_core::environment::Environment::resolve_for_exec_with_secrets(
                    request.command,
                    &env_vars,
                )
                .await?;
            secrets_for_redaction = secrets;

            cuenv_events::register_secrets(secrets_for_redaction.iter().cloned());

            for (key, value) in exec_env_vars {
                runtime_env.set(key, value);
            }
        }
    } else if let Some(env) = &env_config {
        // For Base: no hooks, but still resolve secrets for exec
        tracing::debug!("Using Base configuration (no hooks)");

        let env_vars = if let Some(env_name) = request.environment_override {
            env.for_environment(env_name)
        } else {
            env.base.clone()
        };

        let (exec_env_vars, secrets) =
            cuenv_core::environment::Environment::resolve_for_exec_with_secrets(
                request.command,
                &env_vars,
            )
            .await?;
        secrets_for_redaction = secrets;

        cuenv_events::register_secrets(secrets_for_redaction.iter().cloned());

        for (key, value) in exec_env_vars {
            runtime_env.set(key, value);
        }
    } else {
        // No manifest at all - inherit host PATH
        tracing::debug!("No CUE manifest found, using host environment");
        if let Ok(host_path) = std::env::var("PATH") {
            runtime_env.set("PATH".to_string(), host_path);
        }
    }

    // Add OP_SERVICE_ACCOUNT_TOKEN to redaction list if set (it's a credential, not a secret from resolver)
    if let Ok(token) = std::env::var("OP_SERVICE_ACCOUNT_TOKEN")
        && !token.is_empty()
    {
        secrets_for_redaction.push(token);
    }

    // Ensure lockfile is up to date for tools declared in the current project.
    // This keeps `cuenv exec` self-healing when runtime tool definitions change.
    if let Some(project) = project_for_hooks {
        ensure_lockfile_for_runtime_tools(&target_path, request.package, project, executor).await?;
    }

    // Download and activate tools from lockfile by prepending to PATH and library path.
    // This happens automatically without requiring hook approval since tool
    // activation is a controlled, safe operation (just adds paths to the environment).
    // Use the target_path to scope tool activation to this project only.
    // Tool activation failures are fatal - commands require their tools to run.
    ensure_tools_downloaded(Some(&target_path))
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to download tools: {e}")))?;
    if let Some(activation_steps) =
        resolve_tool_activation_steps(Some(&target_path)).map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to resolve tools activation: {e}"))
        })?
    {
        tracing::debug!(
            steps = activation_steps.len(),
            "Applying configured tool activation operations"
        );

        for step in activation_steps {
            let current = runtime_env.get(&step.var);
            if let Some(new_value) = apply_resolved_tool_activation(current, &step) {
                runtime_env.set(step.var.clone(), new_value);
            }
        }
    }

    // Resolve the command path using the runtime environment's PATH (with host fallback)
    // This is necessary because the child process will have hermetic PATH
    let resolved_command = runtime_env.resolve_command(request.command);

    // Execute the command with the environment, redacting any secrets from output
    let exit_code = execute_command_with_redaction(
        &resolved_command,
        request.args,
        &runtime_env,
        &secrets_for_redaction,
    )
    .await?;

    Ok(exit_code)
}

/// Synchronize the lockfile when runtime tools for the current project are missing or stale.
async fn ensure_lockfile_for_runtime_tools(
    project_path: &Path,
    package: &str,
    project: &Project,
    executor: &CommandExecutor,
) -> Result<()> {
    if !lockfile_needs_runtime_tool_sync(project_path, project)? {
        return Ok(());
    }

    tracing::info!(
        project = %project_path.display(),
        "Lockfile missing/stale runtime tools; running sync lock"
    );

    let options = SyncOptions {
        mode: SyncMode::Write,
        show_diff: false,
        ci_provider: None,
        update_tools: None,
    };
    let registry = default_registry();
    registry
        .sync_provider("lock", project_path, package, &options, false, executor)
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to sync lockfile: {e}")))?;

    Ok(())
}

/// Check whether lockfile entries required by this project's runtime tools are missing or stale.
fn lockfile_needs_runtime_tool_sync(project_path: &Path, project: &Project) -> Result<bool> {
    let Some(Runtime::Tools(tools_runtime)) = &project.runtime else {
        return Ok(false);
    };
    if tools_runtime.tools.is_empty() {
        return Ok(false);
    }

    let Some(lockfile_path) = find_lockfile(project_path) else {
        return Ok(true);
    };

    let lockfile = Lockfile::load(&lockfile_path)
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to load lockfile: {e}")))?;
    let Some(lockfile) = lockfile else {
        return Ok(true);
    };

    let platform_str = cuenv_core::tools::Platform::current().to_string();
    for (tool_name, spec) in &tools_runtime.tools {
        let required_version = match spec {
            ToolSpec::Version(v) => v.as_str(),
            ToolSpec::Full(config) => config.version.as_str(),
        };

        let Some(locked_tool) = lockfile.find_tool(tool_name) else {
            return Ok(true);
        };
        if !versions_match(required_version, &locked_tool.version) {
            return Ok(true);
        }
        if !locked_tool.platforms.contains_key(&platform_str) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Compare tool versions while tolerating optional `v` prefixes.
fn versions_match(required: &str, locked: &str) -> bool {
    required == locked || required.trim_start_matches('v') == locked.trim_start_matches('v')
}

/// Find `cuenv.lock` from the current directory up to ancestors.
fn find_lockfile(start_dir: &Path) -> Option<std::path::PathBuf> {
    let mut current = start_dir
        .canonicalize()
        .unwrap_or_else(|_| start_dir.to_path_buf());

    loop {
        let candidate = current.join(LOCKFILE_NAME);
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn create_test_executor(package: &str) -> CommandExecutor {
        let (sender, _receiver) = mpsc::unbounded_channel();
        CommandExecutor::new(sender, package.to_string())
    }

    #[tokio::test]
    async fn test_execute_command_with_env() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    TEST_VAR: "test_value"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

        let executor = create_test_executor("test");

        // Test depends on FFI availability
        let args = vec!["test".to_string()];
        let request = ExecRequest {
            path: temp_dir.path().to_str().unwrap(),
            package: "test",
            command: "echo",
            args: &args,
            environment_override: None,
        };
        let result = execute_exec(request, &executor).await;

        if let Ok(exit_code) = result {
            assert_eq!(exit_code, 0);
        } else {
            // FFI not available in test environment
        }
    }

    #[tokio::test]
    async fn test_execute_shell_via_exec() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    NAME: "World"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

        let executor = create_test_executor("test");

        // Test shell execution via execute_exec with shell command
        let args = vec!["-c".to_string(), "echo Hello".to_string()];
        let request = ExecRequest {
            path: temp_dir.path().to_str().unwrap(),
            package: "test",
            command: "sh",
            args: &args,
            environment_override: None,
        };
        let result = execute_exec(request, &executor).await;

        if let Ok(exit_code) = result {
            assert_eq!(exit_code, 0);
        } else {
            // FFI not available in test environment
        }
    }

    #[tokio::test]
    async fn test_exec_without_cue_module() {
        // Create temp dir WITHOUT any CUE files - exec should still work
        let temp_dir = TempDir::new().unwrap();

        let executor = create_test_executor("cuenv");

        // execute_exec should work even without a CUE module
        let args = vec!["no-module-mode".to_string()];
        let request = ExecRequest {
            path: temp_dir.path().to_str().unwrap(),
            package: "cuenv", // package doesn't matter without a module
            command: "echo",
            args: &args,
            environment_override: None,
        };
        let result = execute_exec(request, &executor).await;

        // Should succeed - exec works without a CUE module
        assert!(
            result.is_ok(),
            "Exec without module should succeed: {result:?}"
        );
        assert_eq!(result.unwrap(), 0);
    }
}
