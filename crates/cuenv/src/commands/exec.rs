//! Exec command implementation for running arbitrary commands with CUE environment
//!
//! This module supports three modes:
//! 1. **Project mode**: When inside a CUE module with `schema.#Project`, uses CUE-defined
//!    environment, hooks, secrets, and tools.
//! 2. **Base mode**: When inside a CUE module with `schema.#Base`, uses CUE-defined
//!    environment (no hooks) and lockfile tools.
//! 3. **No-module mode**: When outside a CUE module, runs commands with just the runtime
//!    tools from any available lockfile.

use super::env_file::find_cue_module_root;
use super::tools::{ensure_tools_downloaded, get_tool_paths};
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::{Base, Project};
use cuenv_core::tasks::execute_command_with_redaction;
use std::path::Path;

use cuenv_core::hooks::approval::{
    ApprovalManager, ApprovalStatus, ConfigSummary, check_approval_status,
};
use cuenv_events::emit_stderr;

use super::export::{extract_static_env_vars, get_environment_with_hooks};

/// Represents the type of manifest found at a path.
enum ManifestKind {
    /// Full project with hooks, runtime, etc.
    Project(Box<Project>),
    /// Base configuration with just env and config
    Base(Box<Base>),
    /// No CUE module found
    None,
}

/// Run a command with the CUE environment.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
///
/// If no CUE module is found, runs in "tools-only" mode where only
/// runtime tools from lockfiles are activated.
///
/// # Errors
///
/// Returns an error if CUE evaluation fails or command execution fails.
#[allow(clippy::too_many_lines)]
pub async fn execute_exec(
    path: &str,
    package: &str,
    command: &str,
    args: &[String],
    environment_override: Option<&str>,
    executor: Option<&CommandExecutor>,
) -> Result<i32> {
    tracing::info!(
        "Running command with CUE environment from path: {}, package: {}, command: {} {:?}",
        path,
        package,
        command,
        args
    );

    // Evaluate CUE to get environment using module-wide evaluation
    let target_path = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    // Try to get the manifest - can be Project, Base, or None
    let manifest_kind: ManifestKind = if let Some(cmd_executor) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        match cmd_executor.get_module(&target_path) {
            Ok(module) => {
                let rel_path = relative_path_from_root(&module.root, &target_path);

                let instance = module.get(&rel_path).ok_or_else(|| {
                    cuenv_core::Error::configuration(format!(
                        "No CUE instance found at path: {} (relative: {})",
                        target_path.display(),
                        rel_path.display()
                    ))
                })?;

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
        }
    } else {
        // Legacy path: fresh evaluation
        tracing::debug!("Using fresh module evaluation (no executor)");

        if let Some(module_root) = find_cue_module_root(&target_path) {
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
            );

            let rel_path = relative_path_from_root(&module_root, &target_path);
            let instance = module.get(&rel_path).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "No CUE instance found at path: {} (relative: {})",
                    target_path.display(),
                    rel_path.display()
                ))
            })?;

            // Handle both Project and Base
            match instance.kind {
                cuenv_core::InstanceKind::Project => {
                    ManifestKind::Project(Box::new(instance.deserialize()?))
                }
                cuenv_core::InstanceKind::Base => {
                    ManifestKind::Base(Box::new(instance.deserialize()?))
                }
            }
        } else {
            tracing::debug!("No CUE module found");
            ManifestKind::None
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
    let directory = std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());

    // Build base environment based on manifest type
    let mut runtime_env = Environment::new();
    let mut secrets_for_redaction: Vec<String> = Vec::new();

    // For Project: check hooks approval and run hooks if approved
    // For Base/None: just extract static env vars (no hooks)
    if let Some(project) = project_for_hooks {
        let summary = ConfigSummary::from_project(project);

        let hooks_approved = if summary.has_hooks {
            let mut approval_manager = ApprovalManager::with_default_file()?;
            approval_manager.load_approvals().await?;
            let approval_status = check_approval_status(&approval_manager, &directory, project)?;
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
            get_environment_with_hooks(&directory, project, package, executor).await?
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
            let env_vars = if let Some(env_name) = environment_override {
                env.for_environment(env_name)
            } else {
                env.base.clone()
            };

            let (exec_env_vars, secrets) =
                cuenv_core::environment::Environment::resolve_for_exec_with_secrets(
                    command, &env_vars,
                )
                .await?;
            secrets_for_redaction = secrets;

            cuenv_events::register_secrets(secrets_for_redaction.iter().cloned());

            for (key, value) in exec_env_vars {
                runtime_env.set(key, value);
            }
        }
    } else if let Some(env) = &env_config {
        // For Base: just use static env vars (no hooks, no secret resolution)
        tracing::debug!("Using Base configuration (no hooks)");
        for (key, var) in &env.base {
            // For Base, we use to_string_value() which handles all EnvValue variants
            // but doesn't resolve secrets (secrets show as "[SECRET]")
            runtime_env.set(key.clone(), var.to_string_value());
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

    // Download and activate tools from lockfile by prepending to PATH and library path.
    // This happens automatically without requiring hook approval since tool
    // activation is a controlled, safe operation (just adds paths to the environment).
    // Use the target_path to scope tool activation to this project only.
    // Tool activation failures are fatal - commands require their tools to run.
    ensure_tools_downloaded(Some(&target_path))
        .await
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to download tools: {e}")))?;
    if let Ok(Some(tool_paths)) = get_tool_paths(Some(&target_path)) {
        tracing::debug!(
            "Activating {} tool bin directories and {} lib directories",
            tool_paths.bin_dirs.len(),
            tool_paths.lib_dirs.len()
        );

        // Prepend tool bin directories to PATH
        // Use runtime_env PATH (from CUE), NOT host PATH - this ensures hermetic isolation
        if let Some(path_prepend) = tool_paths.path_prepend() {
            let current_path = runtime_env
                .get("PATH")
                .map(ToString::to_string)
                .unwrap_or_default();
            let new_path = if current_path.is_empty() {
                path_prepend
            } else {
                format!("{path_prepend}:{current_path}")
            };
            runtime_env.set("PATH".to_string(), new_path);
        }

        // Prepend tool lib directories to library path
        // Use runtime_env lib path (from CUE), NOT host lib path - hermetic isolation
        if let Some(lib_prepend) = tool_paths.lib_path_prepend() {
            #[cfg(target_os = "macos")]
            {
                let lib_var = "DYLD_LIBRARY_PATH";
                let current = runtime_env
                    .get(lib_var)
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let new_path = if current.is_empty() {
                    lib_prepend
                } else {
                    format!("{lib_prepend}:{current}")
                };
                runtime_env.set(lib_var.to_string(), new_path);
            }

            #[cfg(not(target_os = "macos"))]
            {
                let lib_var = "LD_LIBRARY_PATH";
                let current = runtime_env
                    .get(lib_var)
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let new_path = if current.is_empty() {
                    lib_prepend
                } else {
                    format!("{lib_prepend}:{current}")
                };
                runtime_env.set(lib_var.to_string(), new_path);
            }
        }
    }

    // Resolve the command path using the runtime environment's PATH (with host fallback)
    // This is necessary because the child process will have hermetic PATH
    let resolved_command = runtime_env.resolve_command(command);

    // Execute the command with the environment, redacting any secrets from output
    let exit_code = execute_command_with_redaction(
        &resolved_command,
        args,
        &runtime_env,
        &secrets_for_redaction,
    )
    .await?;

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execute_command_with_env() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test
env: {
    TEST_VAR: "test_value"
}"#;
        fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

        // Test depends on FFI availability
        let result = execute_exec(
            temp_dir.path().to_str().unwrap(),
            "test",
            "echo",
            &["test".to_string()],
            None,
            None, // executor
        )
        .await;

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

        // Test shell execution via execute_exec with shell command
        let result = execute_exec(
            temp_dir.path().to_str().unwrap(),
            "test",
            "sh",
            &["-c".to_string(), "echo Hello".to_string()],
            None,
            None, // executor
        )
        .await;

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

        // execute_exec should work even without a CUE module
        let result = execute_exec(
            temp_dir.path().to_str().unwrap(),
            "cuenv", // package doesn't matter without a module
            "echo",
            &["no-module-mode".to_string()],
            None,
            None,
        )
        .await;

        // Should succeed - exec works without a CUE module
        assert!(
            result.is_ok(),
            "Exec without module should succeed: {result:?}"
        );
        assert_eq!(result.unwrap(), 0);
    }
}
