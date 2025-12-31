//! Command handler traits and implementations for lifecycle event management.
//!
//! This module provides the `CommandHandler` trait that standardizes command
//! execution with automatic event emission (start, complete), eliminating
//! boilerplate in individual command implementations.

use async_trait::async_trait;
use cuenv_core::Result;

use crate::cli::{ShellType, StatusFormat};
use crate::events::Event;

use super::{CommandExecutor, ci, env, exec, export, hooks, sync, task};

/// Trait for commands with lifecycle events.
///
/// Commands implementing this trait get automatic event emission
/// (`CommandStart`, `CommandComplete`) when executed through `CommandRunner`.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// The unique name of this command for event tracking (e.g., "env print").
    fn command_name(&self) -> &'static str;

    /// Execute the command and return output string.
    async fn execute(&self, executor: &CommandExecutor) -> Result<String>;

    /// Whether to print output to stdout (default: true for non-empty output).
    fn should_print_output(&self) -> bool {
        true
    }
}

/// Extension trait for running commands with event lifecycle.
#[async_trait]
pub trait CommandRunner {
    /// Run a command with automatic event lifecycle management.
    async fn run_command<C: CommandHandler>(&self, cmd: C) -> Result<()>;
}

#[async_trait]
impl CommandRunner for CommandExecutor {
    async fn run_command<C: CommandHandler>(&self, cmd: C) -> Result<()> {
        let name = cmd.command_name();

        self.send_event(Event::CommandStart {
            command: name.to_string(),
        });

        match cmd.execute(self).await {
            Ok(output) => {
                if cmd.should_print_output() && !output.is_empty() {
                    println!("{output}");
                }
                self.send_event(Event::CommandComplete {
                    command: name.to_string(),
                    success: true,
                    output,
                });
                Ok(())
            }
            Err(e) => {
                self.send_event(Event::CommandComplete {
                    command: name.to_string(),
                    success: false,
                    output: format!("Error: {e}"),
                });
                Err(e)
            }
        }
    }
}

// ============================================================================
// Command Handler Implementations
// ============================================================================

/// Handler for `env print` command.
pub struct EnvPrintHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Output format (e.g., "json", "yaml", "table").
    pub format: String,
    /// Optional environment name to use for evaluation.
    pub environment: Option<String>,
}

#[async_trait]
impl CommandHandler for EnvPrintHandler {
    fn command_name(&self) -> &'static str {
        "env print"
    }

    fn should_print_output(&self) -> bool {
        false // env::execute_env_print handles printing
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        env::execute_env_print(
            &self.path,
            &self.package,
            &self.format,
            self.environment.as_deref(),
            Some(executor),
        )
        .await
    }
}

/// Handler for `env list` command.
pub struct EnvListHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Output format (e.g., "json", "yaml", "table").
    pub format: String,
}

#[async_trait]
impl CommandHandler for EnvListHandler {
    fn command_name(&self) -> &'static str {
        "env list"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        env::execute_env_list(&self.path, &self.package, &self.format, Some(executor)).await
    }
}

/// Handler for `env load` command.
pub struct EnvLoadHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
}

#[async_trait]
impl CommandHandler for EnvLoadHandler {
    fn command_name(&self) -> &'static str {
        "env load"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        hooks::execute_env_load(&self.path, &self.package, Some(executor)).await
    }
}

/// Handler for `env status` command.
pub struct EnvStatusHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Whether to wait for the environment to become ready.
    pub wait: bool,
    /// Maximum time in seconds to wait for environment readiness.
    pub timeout: u64,
    /// Output format for the status information.
    pub format: StatusFormat,
}

#[async_trait]
impl CommandHandler for EnvStatusHandler {
    fn command_name(&self) -> &'static str {
        "env status"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        hooks::execute_env_status(
            &self.path,
            &self.package,
            self.wait,
            self.timeout,
            self.format,
            Some(executor),
        )
        .await
    }
}

/// Handler for `env check` command.
pub struct EnvCheckHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Shell type to check compatibility with.
    pub shell: ShellType,
}

#[async_trait]
impl CommandHandler for EnvCheckHandler {
    fn command_name(&self) -> &'static str {
        "env check"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        hooks::execute_env_check(&self.path, &self.package, self.shell, Some(executor)).await
    }
}

/// Handler for `env inspect` command.
pub struct EnvInspectHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
}

#[async_trait]
impl CommandHandler for EnvInspectHandler {
    fn command_name(&self) -> &'static str {
        "env inspect"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        hooks::execute_env_inspect(&self.path, &self.package, Some(executor)).await
    }
}

/// Handler for `allow` command.
pub struct AllowHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Optional note explaining why this project is allowed.
    pub note: Option<String>,
    /// Skip confirmation prompt and automatically approve.
    pub yes: bool,
}

#[async_trait]
impl CommandHandler for AllowHandler {
    fn command_name(&self) -> &'static str {
        "allow"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        hooks::execute_allow(
            &self.path,
            &self.package,
            self.note.clone(),
            self.yes,
            Some(executor),
        )
        .await
    }
}

/// Handler for `deny` command.
pub struct DenyHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Deny all previously allowed projects.
    pub all: bool,
}

#[async_trait]
impl CommandHandler for DenyHandler {
    fn command_name(&self) -> &'static str {
        "deny"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, _executor: &CommandExecutor) -> Result<String> {
        hooks::execute_deny(&self.path, &self.package, self.all).await
    }
}

/// Handler for `export` command.
pub struct ExportHandler {
    /// Optional shell type override for export format.
    pub shell: Option<String>,
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
}

#[async_trait]
impl CommandHandler for ExportHandler {
    fn command_name(&self) -> &'static str {
        "export"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        export::execute_export(
            self.shell.as_deref(),
            &self.path,
            &self.package,
            Some(executor),
        )
        .await
    }
}

/// Handler for `exec` command.
pub struct ExecHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Command to execute within the environment.
    pub command: String,
    /// Arguments to pass to the command.
    pub args: Vec<String>,
    /// Optional environment name to use for execution.
    pub environment: Option<String>,
}

#[async_trait]
impl CommandHandler for ExecHandler {
    fn command_name(&self) -> &'static str {
        "exec"
    }

    fn should_print_output(&self) -> bool {
        false
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        let exit_code = exec::execute_exec(
            &self.path,
            &self.package,
            &self.command,
            &self.args,
            self.environment.as_deref(),
            Some(executor),
        )
        .await?;

        if exit_code == 0 {
            Ok(format!("Command exited with code {exit_code}"))
        } else {
            Err(cuenv_core::Error::configuration(format!(
                "Command failed with exit code {exit_code}"
            )))
        }
    }
}

/// Handler for `task` command.
#[allow(clippy::struct_excessive_bools)]
pub struct TaskHandler {
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Optional specific task name to execute.
    pub name: Option<String>,
    /// Labels to filter tasks by.
    pub labels: Vec<String>,
    /// Optional environment name to use for task execution.
    pub environment: Option<String>,
    /// Output format (e.g., "json", "yaml", "table").
    pub format: String,
    /// Optional path to materialize task outputs to.
    pub materialize_outputs: Option<String>,
    /// Whether to display cache paths for tasks.
    pub show_cache_path: bool,
    /// Optional execution backend override (e.g., "dagger").
    pub backend: Option<String>,
    /// Whether to use the TUI interface.
    pub tui: bool,
    /// Whether to run in interactive mode for task selection.
    pub interactive: bool,
    /// Whether to show help for the specified task.
    pub help: bool,
    /// Whether to run all tasks.
    pub all: bool,
    /// Whether to skip task dependencies.
    pub skip_dependencies: bool,
    /// Additional arguments to pass to the task.
    pub task_args: Vec<String>,
}

#[async_trait]
impl CommandHandler for TaskHandler {
    fn command_name(&self) -> &'static str {
        "task"
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        // Validate conflicting selection modes before constructing the request
        if !self.labels.is_empty() && self.name.is_some() {
            return Err(cuenv_core::Error::configuration(
                "Cannot specify both a task name and --label",
            ));
        }
        if !self.labels.is_empty() && !self.task_args.is_empty() {
            return Err(cuenv_core::Error::configuration(
                "Task arguments are not supported when selecting tasks by label",
            ));
        }

        // Build request using the builder pattern
        let mut request = match (&self.name, &self.labels, self.interactive, self.all) {
            (Some(name), _, _, _) => {
                task::TaskExecutionRequest::named(&self.path, &self.package, name)
                    .with_args(self.task_args.clone())
            }
            (None, labels, _, _) if !labels.is_empty() => {
                task::TaskExecutionRequest::labels(&self.path, &self.package, labels.clone())
            }
            (None, _, true, _) => {
                task::TaskExecutionRequest::interactive(&self.path, &self.package)
            }
            (None, _, _, true) => task::TaskExecutionRequest::all(&self.path, &self.package),
            (None, _, _, _) => task::TaskExecutionRequest::list(&self.path, &self.package),
        };

        // Apply optional settings
        if let Some(env) = &self.environment {
            request = request.with_environment(env);
        }
        request = request.with_format(&self.format);
        if let Some(path) = &self.materialize_outputs {
            request = request.with_materialize_outputs(path);
        }
        if self.show_cache_path {
            request = request.with_show_cache_path();
        }
        if let Some(backend) = &self.backend {
            request = request.with_backend(backend);
        }
        if self.tui {
            request = request.with_tui();
        }
        if self.help {
            request = request.with_help();
        }
        if self.skip_dependencies {
            request = request.with_skip_dependencies();
        }

        let request = request.with_executor(executor);
        task::execute(request).await
    }
}

/// Handler for `ci` command.
pub struct CiHandler {
    /// Whether to run in dry-run mode without executing.
    pub dry_run: bool,
    /// Optional pipeline name to execute.
    pub pipeline: Option<String>,
    /// Optional dynamic configuration source.
    pub dynamic: Option<String>,
    /// Optional starting point for pipeline execution.
    pub from: Option<String>,
}

#[async_trait]
impl CommandHandler for CiHandler {
    fn command_name(&self) -> &'static str {
        "ci"
    }

    fn should_print_output(&self) -> bool {
        false // CI handles its own output
    }

    async fn execute(&self, _executor: &CommandExecutor) -> Result<String> {
        ci::execute_ci(
            self.dry_run,
            self.pipeline.clone(),
            self.dynamic.clone(),
            self.from.clone(),
        )
        .await?;
        Ok("CI execution completed".to_string())
    }
}

/// Scope of sync operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SyncScope {
    /// Sync single path.
    #[default]
    Path,
    /// Sync all projects in workspace.
    Workspace,
}

/// Handler for `sync` command using provider registry.
pub struct SyncHandler {
    /// Specific provider name (None = sync all providers).
    pub subcommand: Option<String>,
    /// Path to the cuenv project directory.
    pub path: String,
    /// Name of the CUE package to evaluate.
    pub package: String,
    /// Operation mode (write, dry-run, check).
    pub mode: sync::SyncMode,
    /// Scope (single path or entire workspace).
    pub scope: SyncScope,
    /// Show diff for cubes (cubes-specific).
    pub show_diff: bool,
    /// CI provider filter (github, buildkite).
    pub ci_provider: Option<String>,
}

#[async_trait]
impl CommandHandler for SyncHandler {
    fn command_name(&self) -> &'static str {
        "sync"
    }

    async fn execute(&self, executor: &CommandExecutor) -> Result<String> {
        use sync::{SyncOptions, default_registry};

        let registry = default_registry();
        let options = SyncOptions {
            mode: self.mode.clone(),
            show_diff: self.show_diff,
            ci_provider: self.ci_provider.clone(),
        };

        let path = std::path::Path::new(&self.path);
        let sync_all = self.scope == SyncScope::Workspace;
        let project_error = |path: &std::path::Path| {
            cuenv_core::Error::configuration(format!(
                "No cuenv project found at path: {}. Run 'cuenv info' to inspect project layout or use 'cuenv sync -A' to sync all projects.",
                path.display()
            ))
        };

        if self.subcommand.is_none() && !sync_all {
            let target_path = path.canonicalize().map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(path.to_path_buf().into_boxed_path()),
                operation: "canonicalize path".to_string(),
            })?;
            let (is_project, is_root) = {
                let module = executor.get_module(&target_path)?;
                let is_root = module.root == target_path;
                let is_project = module.projects().any(|instance| {
                    module
                        .root
                        .join(&instance.path)
                        .canonicalize()
                        .ok()
                        .is_some_and(|path| path == target_path)
                });
                (is_project, is_root)
            };

            if !is_project {
                if !is_root {
                    return Err(project_error(path));
                }

                let provider_names = ["ignore", "codeowners"];
                let mut outputs = Vec::new();
                let mut had_error = false;

                for name in provider_names {
                    let result = registry
                        .sync_provider(name, path, &self.package, &options, sync_all, executor)
                        .await;

                    match result {
                        Ok(r) => {
                            if !r.output.is_empty() {
                                outputs.push(format!("[{name}]\n{}", r.output));
                            }
                            had_error |= r.had_error;
                        }
                        Err(e) => {
                            outputs.push(format!("[{name}] Error: {e}"));
                            had_error = true;
                        }
                    }
                }

                let combined = outputs.join("\n\n");
                return if had_error {
                    Err(cuenv_core::Error::configuration(combined))
                } else if combined.is_empty() {
                    Ok("No sync operations performed.".to_string())
                } else {
                    Ok(combined)
                };
            }
        }

        match &self.subcommand {
            // Specific provider: cuenv sync cubes
            Some(name) => {
                let result = registry
                    .sync_provider(name, path, &self.package, &options, sync_all, executor)
                    .await?;
                Ok(result.output)
            }
            // All providers: cuenv sync or cuenv sync -A
            None => {
                registry
                    .sync_all(path, &self.package, &options, sync_all, executor)
                    .await
            }
        }
    }
}

/// Handler for `shell init` command (synchronous).
pub struct ShellInitHandler {
    /// Shell type to generate initialization script for.
    pub shell: ShellType,
}

impl ShellInitHandler {
    /// Execute shell init synchronously (doesn't use async trait)
    pub fn execute_sync(&self, executor: &CommandExecutor) {
        let name = "shell init";
        executor.send_event(Event::CommandStart {
            command: name.to_string(),
        });

        let output = hooks::execute_shell_init(self.shell);

        executor.send_event(Event::CommandComplete {
            command: name.to_string(),
            success: true,
            output,
        });
    }
}
