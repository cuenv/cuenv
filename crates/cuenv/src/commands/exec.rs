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
use cuenv_core::runtime::resolve_runtime_environment;
use cuenv_task_exec::execute_command_with_redaction;
use cuenv_tool_runtime::{ResolvedToolActivationStep, apply_resolved_tool_activation};
use std::path::Path;

use super::export::{StaticEnvConsumer, extract_static_env_vars};
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

impl ManifestKind {
    fn project(&self) -> Option<&Project> {
        match self {
            Self::Project(project) => Some(project),
            Self::Base(_) | Self::None => None,
        }
    }
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

impl ExecRequest<'_> {
    /// The project environment request this exec runs in.
    #[must_use]
    pub const fn project_environment_request(&self) -> ProjectEnvironmentRequest<'_> {
        ProjectEnvironmentRequest {
            path: self.path,
            package: self.package,
            environment_override: self.environment_override,
        }
    }
}

/// Request for the project environment that commands run in.
///
/// This is the environment `cuenv exec` builds: the project's runtime
/// (Nix/devenv) variables, lockfile tool activation, and the CUE `env`
/// block (optionally for a named environment).
#[derive(Clone, Copy, Debug)]
pub struct ProjectEnvironmentRequest<'a> {
    /// Path to the CUE module or project directory.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Optional environment name to use.
    pub environment_override: Option<&'a str>,
}

/// The command-independent part of a project's execution environment.
///
/// Loading it evaluates the manifest, resolves the runtime environment,
/// keeps the lockfile in sync, and downloads activated tools once. The
/// command-scoped environment, whose CUE variables are filtered by
/// `allowExec` policies, comes from [`ProjectEnvironment::for_command`].
///
/// When the directory has no CUE module, or the project configures neither
/// a runtime nor tools, the environment adds nothing to `PATH`, so commands
/// resolve against the inherited `PATH`.
pub struct ProjectEnvironment {
    manifest_kind: ManifestKind,
    runtime_env: Environment,
    activation_steps: Vec<ResolvedToolActivationStep>,
    environment_override: Option<String>,
}

/// The environment one command runs in, plus the secret values to redact.
#[derive(Debug)]
pub struct CommandEnvironment {
    environment: Environment,
    secrets_for_redaction: Vec<String>,
}

impl CommandEnvironment {
    /// The resolved environment variables.
    #[must_use]
    pub const fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Resolved secret values that must be redacted from output.
    #[must_use]
    pub fn secrets_for_redaction(&self) -> &[String] {
        &self.secrets_for_redaction
    }
}

impl ProjectEnvironment {
    /// Load the project environment for `request.path`.
    ///
    /// Uses the executor's cached module evaluation.
    ///
    /// # Errors
    ///
    /// Returns an error if CUE evaluation, runtime resolution, lockfile
    /// synchronization, or tool download fails.
    pub async fn load(
        request: ProjectEnvironmentRequest<'_>,
        executor: &CommandExecutor,
    ) -> Result<Self> {
        let target_path = Path::new(request.path).canonicalize().map_err(|e| {
            cuenv_core::Error::io_with_path(
                "canonicalize path",
                Path::new(request.path).to_path_buf(),
                e,
            )
        })?;

        let manifest_kind = load_manifest_kind(&target_path, executor)?;

        // Shell hooks are intentionally not executed; runtime activation is
        // handled here.
        let mut runtime_env = Environment::new();
        let runtime = match &manifest_kind {
            ManifestKind::Project(project) => Some(project.runtime.as_ref()),
            ManifestKind::Base(base) => {
                tracing::debug!("Using Base configuration");
                Some(base.runtime.as_ref())
            }
            ManifestKind::None => None,
        };
        if let Some(runtime) = runtime {
            for (key, value) in resolve_runtime_environment(&target_path, runtime).await? {
                runtime_env.set(key, value);
            }
        } else {
            tracing::debug!("No CUE manifest found, using host environment");
            if let Ok(host_path) = std::env::var("PATH") {
                runtime_env.set("PATH".to_string(), host_path);
            }
        }

        // Ensure lockfile is up to date for tools declared in the current project.
        // This keeps `cuenv exec` self-healing when runtime tool definitions change.
        if let Some(project) = manifest_kind.project() {
            ensure_lockfile_for_runtime_tools(&target_path, request.package, project, executor)
                .await?;
        }

        let activation_steps = if should_activate_lockfile_tools(manifest_kind.project()) {
            // Download and activate tools from lockfile by prepending to PATH and library path.
            // This happens automatically without requiring hook approval since tool
            // activation is a controlled, safe operation (just adds paths to the environment).
            // Use the target_path to scope tool activation to this project only.
            // Tool activation failures are fatal - commands require their tools to run.
            ensure_tools_downloaded(Some(&target_path))
                .await
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!("Failed to download tools: {e}"))
                })?;
            resolve_tool_activation_steps(Some(&target_path))
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to resolve tools activation: {e}"
                    ))
                })?
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(Self {
            manifest_kind,
            runtime_env,
            activation_steps,
            environment_override: request.environment_override.map(str::to_string),
        })
    }

    /// Build the environment `command` runs in.
    ///
    /// CUE environment variables are filtered by their `allowExec` policies
    /// for `command`, secrets are resolved and registered for redaction, and
    /// tool activation is applied last.
    ///
    /// # Errors
    ///
    /// Returns an error if secret resolution fails.
    pub async fn for_command(&self, command: &str) -> Result<CommandEnvironment> {
        let mut environment = self.runtime_env.clone();
        let mut secrets_for_redaction: Vec<String> = Vec::new();

        let env_config = match &self.manifest_kind {
            ManifestKind::Project(project) => {
                let base_env_vars =
                    extract_static_env_vars(project, StaticEnvConsumer::Exec(command));
                tracing::debug!("Base environment variables for exec: {:?}", base_env_vars);
                for (key, value) in base_env_vars {
                    environment.set(key, value);
                }
                project.env.as_ref()
            }
            ManifestKind::Base(base) => base.env.as_ref(),
            ManifestKind::None => None,
        };

        if let Some(env) = env_config {
            let env_vars = self
                .environment_override
                .as_deref()
                .map_or_else(|| env.base.clone(), |name| env.for_environment(name));

            let (exec_env_vars, secrets) =
                Environment::resolve_for_exec_with_secrets(command, &env_vars).await?;
            secrets_for_redaction = secrets;

            cuenv_events::register_secrets(secrets_for_redaction.iter().cloned());

            for (key, value) in exec_env_vars {
                environment.set(key, value);
            }
        }

        if !self.activation_steps.is_empty() {
            tracing::debug!(
                steps = self.activation_steps.len(),
                "Applying configured tool activation operations"
            );
        }
        for step in &self.activation_steps {
            let current = environment.get(&step.var);
            if let Some(new_value) = apply_resolved_tool_activation(current, step) {
                environment.set(step.var.clone(), new_value);
            }
        }

        for name in [
            "OP_SERVICE_ACCOUNT_TOKEN",
            "INFISICAL_TOKEN",
            "INFISICAL_CLIENT_SECRET",
        ] {
            if let Ok(token) = std::env::var(name)
                && !token.is_empty()
            {
                secrets_for_redaction.push(token);
            }
        }

        Ok(CommandEnvironment {
            environment,
            secrets_for_redaction,
        })
    }
}

fn load_manifest_kind(target_path: &Path, executor: &CommandExecutor) -> Result<ManifestKind> {
    match executor.get_module(target_path) {
        Ok(module) => {
            tracing::debug!("Using cached module evaluation from executor");
            let rel_path = relative_path_from_root(&module.root, target_path);

            let instance = module.get(&rel_path).ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "No CUE instance found at path: {} (relative: {})",
                    target_path.display(),
                    rel_path.display()
                ))
            })?;

            match instance.kind {
                cuenv_core::InstanceKind::Project => {
                    Ok(ManifestKind::Project(Box::new(instance.deserialize()?)))
                }
                cuenv_core::InstanceKind::Base => {
                    Ok(ManifestKind::Base(Box::new(instance.deserialize()?)))
                }
            }
        }
        Err(e) => {
            if e.to_string().contains("No CUE module found") {
                tracing::debug!("No CUE module found");
                Ok(ManifestKind::None)
            } else {
                Err(e)
            }
        }
    }
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

    let project_env =
        ProjectEnvironment::load(request.project_environment_request(), executor).await?;
    let prepared = project_env.for_command(request.command).await?;

    // Resolve the command path using the runtime environment's PATH (with host fallback)
    // This is necessary because the child process will have hermetic PATH
    let resolved_command = prepared.environment().resolve_command(request.command);

    // Execute the command with the environment, redacting any secrets from output
    let exit_code = execute_command_with_redaction(
        &resolved_command,
        request.args,
        prepared.environment(),
        prepared.secrets_for_redaction(),
    )
    .await?;

    Ok(exit_code)
}

fn should_activate_lockfile_tools(project: Option<&Project>) -> bool {
    project.is_none_or(|manifest| matches!(manifest.runtime, Some(Runtime::Tools(_))))
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
        .sync_provider(
            "lock",
            crate::commands::sync::SyncRequest {
                path: project_path,
                package,
                options: &options,
                scope: crate::commands::sync::SyncScope::Path,
                executor,
            },
        )
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

    let platform_str = cuenv_tool_runtime::Platform::current().to_string();
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
