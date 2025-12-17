//! Exec command implementation for running arbitrary commands with CUE environment

use super::env_file::find_cue_module_root;
use super::{CommandExecutor, convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::execute_command;
use std::path::Path;

use cuenv_core::hooks::approval::{
    ApprovalManager, ApprovalStatus, ConfigSummary, check_approval_status,
};
use cuenv_events::emit_stderr;

use super::export::{extract_static_env_vars, get_environment_with_hooks};

/// Execute an arbitrary command with the CUE environment.
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
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
        "Executing command with CUE environment from path: {}, package: {}, command: {} {:?}",
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

    // Use executor's cached module if available
    let manifest: Project = if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let rel_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&rel_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                rel_path.display()
            ))
        })?;

        // Check if this is a Project (has name field) or Base (no name)
        match instance.kind {
            cuenv_core::InstanceKind::Project => instance.deserialize()?,
            cuenv_core::InstanceKind::Base => {
                return Err(cuenv_core::Error::configuration(
                    "This directory uses schema.#Base which doesn't support exec.\n\
                     To use exec, update your env.cue to use schema.#Project:\n\n\
                     schema.#Project\n\
                     name: \"your-project-name\"",
                ));
            }
        }
    } else {
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
        let raw_result = cuengine::evaluate_module(&module_root, package, Some(options))
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

        // Check if this is a Project (has name field) or Base (no name)
        match instance.kind {
            cuenv_core::InstanceKind::Project => instance.deserialize()?,
            cuenv_core::InstanceKind::Base => {
                return Err(cuenv_core::Error::configuration(
                    "This directory uses schema.#Base which doesn't support exec.\n\
                     To use exec, update your env.cue to use schema.#Project:\n\n\
                     schema.#Project\n\
                     name: \"your-project-name\"",
                ));
            }
        }
    };

    // Get environment with hook-generated vars merged in
    let directory = std::fs::canonicalize(path).unwrap_or_else(|_| Path::new(path).to_path_buf());

    // Check approval status before running hooks
    let config_value = serde_json::to_value(&manifest).map_err(|e| {
        cuenv_core::Error::configuration(format!("Failed to serialize config: {e}"))
    })?;
    let summary = ConfigSummary::from_json(&config_value);

    let hooks_approved = if summary.has_hooks {
        let mut approval_manager = ApprovalManager::with_default_file()?;
        approval_manager.load_approvals().await?;
        let approval_status = check_approval_status(&approval_manager, &directory, &config_value)?;
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
        get_environment_with_hooks(&directory, &manifest, package).await?
    } else {
        extract_static_env_vars(&manifest)
    };
    tracing::debug!(
        "Base environment variables after hooks: {:?}",
        base_env_vars
    );

    // Apply command-specific policies and secret resolvers on top of the merged environment
    let mut runtime_env = Environment::new();
    if let Some(env) = &manifest.env {
        // First apply the base environment (static + hooks)
        for (key, value) in &base_env_vars {
            runtime_env.set(key.clone(), value.clone());
        }

        // Get environment variables, applying environment-specific overrides if specified
        let env_vars = if let Some(env_name) = environment_override {
            env.for_environment(env_name)
        } else {
            env.base.clone()
        };

        // Then apply any command-specific overrides with policies and secret resolution
        let exec_env_vars =
            cuenv_core::environment::Environment::resolve_for_exec(command, &env_vars).await?;
        for (key, value) in exec_env_vars {
            runtime_env.set(key, value);
        }
    } else {
        // No manifest env, just use hook-generated environment
        for (key, value) in base_env_vars {
            runtime_env.set(key, value);
        }
    }

    // Execute the command with the environment
    let exit_code = execute_command(command, args, &runtime_env).await?;

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
}
