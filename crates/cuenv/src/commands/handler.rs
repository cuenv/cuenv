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

/// Handler for `env print` command
pub struct EnvPrintHandler {
    pub path: String,
    pub package: String,
    pub format: String,
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

/// Handler for `env list` command
pub struct EnvListHandler {
    pub path: String,
    pub package: String,
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

/// Handler for `env load` command
pub struct EnvLoadHandler {
    pub path: String,
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

/// Handler for `env status` command
pub struct EnvStatusHandler {
    pub path: String,
    pub package: String,
    pub wait: bool,
    pub timeout: u64,
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

/// Handler for `env check` command
pub struct EnvCheckHandler {
    pub path: String,
    pub package: String,
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

/// Handler for `env inspect` command
pub struct EnvInspectHandler {
    pub path: String,
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

/// Handler for `allow` command
pub struct AllowHandler {
    pub path: String,
    pub package: String,
    pub note: Option<String>,
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

/// Handler for `deny` command
pub struct DenyHandler {
    pub path: String,
    pub package: String,
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

/// Handler for `export` command
pub struct ExportHandler {
    pub shell: Option<String>,
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
        export::execute_export(self.shell.as_deref(), &self.package, Some(executor)).await
    }
}

/// Handler for `exec` command
pub struct ExecHandler {
    pub path: String,
    pub package: String,
    pub command: String,
    pub args: Vec<String>,
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

/// Handler for `task` command
#[allow(clippy::struct_excessive_bools)]
pub struct TaskHandler {
    pub path: String,
    pub package: String,
    pub name: Option<String>,
    pub labels: Vec<String>,
    pub environment: Option<String>,
    pub format: String,
    pub materialize_outputs: Option<String>,
    pub show_cache_path: bool,
    pub backend: Option<String>,
    pub tui: bool,
    pub interactive: bool,
    pub help: bool,
    pub all: bool,
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

        let request = task::TaskExecutionRequest::from_legacy(
            &self.path,
            &self.package,
            self.name.as_deref(),
            &self.labels,
            self.environment.as_deref(),
            &self.format,
            false,
            self.materialize_outputs.as_deref(),
            self.show_cache_path,
            self.backend.as_deref(),
            self.tui,
            self.interactive,
            self.help,
            self.all,
            &self.task_args,
            Some(executor),
        );

        task::execute(request).await
    }
}

/// Handler for `ci` command
pub struct CiHandler {
    pub dry_run: bool,
    pub pipeline: Option<String>,
    pub dynamic: Option<String>,
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
    /// Specific provider name (None = sync all providers)
    pub subcommand: Option<String>,
    pub path: String,
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

/// Handler for `shell init` command (synchronous)
pub struct ShellInitHandler {
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
