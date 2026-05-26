use super::{Command, CommandExecutor, handler};
use cuenv_core::Result;
use handler::CommandRunner;

impl CommandExecutor {
    /// Execute a command with automatic event lifecycle management.
    ///
    /// # Errors
    ///
    /// Returns an error if the command execution fails. The specific error
    /// depends on the command being executed:
    /// - Configuration errors for invalid paths or packages
    /// - CUE evaluation errors for malformed CUE files
    /// - Task execution errors for failed commands
    /// - I/O errors for file system operations
    pub async fn execute(&self, command: Command) -> Result<()> {
        match command {
            Command::Version { format: _ } => self.execute_version().await,
            command @ (Command::EnvPrint { .. }
            | Command::EnvList { .. }
            | Command::EnvLoad { .. }
            | Command::EnvStatus { .. }
            | Command::EnvCheck { .. }
            | Command::EnvInspect { .. }) => self.execute_env_command(command).await,
            command @ (Command::Allow { .. }
            | Command::Deny { .. }
            | Command::Export { .. }
            | Command::Exec { .. }
            | Command::Task { .. }
            | Command::Ci { .. }
            | Command::Sync { .. }
            | Command::ShellInit { .. }) => self.execute_handler_command(command).await,
            Command::Web { .. }
            | Command::Completions { .. }
            | Command::Info { .. }
            | Command::Fmt { .. }
            | Command::ChangesetAdd { .. }
            | Command::ChangesetStatus { .. }
            | Command::ChangesetFromCommits { .. }
            | Command::ReleasePrepare { .. }
            | Command::ReleaseVersion { .. }
            | Command::ReleasePublish { .. }
            | Command::ReleaseBinaries { .. }
            | Command::SecretsSetup { .. }
            | Command::RuntimeOciActivate
            | Command::ToolsDownload
            | Command::ToolsActivate
            | Command::ToolsList
            | Command::Build { .. }
            | Command::Up { .. }
            | Command::Down { .. }
            | Command::Logs { .. }
            | Command::Ps { .. }
            | Command::Restart { .. } => Ok(()),
        }
    }

    async fn execute_env_command(&self, command: Command) -> Result<()> {
        match command {
            Command::EnvPrint {
                path,
                package,
                format,
                environment,
            } => {
                self.run_command(handler::EnvPrintHandler {
                    path,
                    package,
                    format,
                    environment,
                })
                .await
            }
            Command::EnvList {
                path,
                package,
                format,
            } => {
                self.run_command(handler::EnvListHandler {
                    path,
                    package,
                    format,
                })
                .await
            }
            Command::EnvLoad { path, package } => {
                self.run_command(handler::EnvLoadHandler { path, package })
                    .await
            }
            Command::EnvStatus {
                path,
                package,
                wait,
                timeout,
                format,
            } => {
                self.run_command(handler::EnvStatusHandler {
                    path,
                    package,
                    wait,
                    timeout,
                    format,
                })
                .await
            }
            Command::EnvCheck {
                path,
                package,
                shell,
            } => {
                self.run_command(handler::EnvCheckHandler {
                    path,
                    package,
                    shell,
                })
                .await
            }
            Command::EnvInspect { path, package } => {
                self.run_command(handler::EnvInspectHandler { path, package })
                    .await
            }
            _ => unreachable!("environment dispatch called for another command family"),
        }
    }

    async fn execute_handler_command(&self, command: Command) -> Result<()> {
        match command {
            Command::Allow {
                path,
                package,
                note,
                yes,
            } => {
                self.run_command(handler::AllowHandler {
                    path,
                    package,
                    note,
                    yes,
                })
                .await
            }
            Command::Deny { path, package } => {
                self.run_command(handler::DenyHandler { path, package })
                    .await
            }
            Command::Export {
                shell,
                path,
                package,
            } => {
                self.run_command(handler::ExportHandler {
                    shell,
                    path,
                    package,
                })
                .await
            }
            Command::Exec {
                path,
                package,
                command,
                args,
                environment,
            } => {
                self.run_command(handler::ExecHandler {
                    path,
                    package,
                    command,
                    args,
                    environment,
                })
                .await
            }
            command @ Command::Task { .. } => self.execute_task_handler(command).await,
            Command::Ci { args } => self.run_command(handler::CiHandler { args }).await,
            Command::Sync {
                subcommand,
                path,
                package,
                mode,
                scope,
                show_diff,
                ci_provider,
                update_tools,
            } => {
                self.run_command(handler::SyncHandler {
                    subcommand,
                    path,
                    package,
                    mode,
                    scope,
                    show_diff,
                    ci_provider,
                    update_tools,
                })
                .await
            }
            Command::ShellInit { shell } => {
                handler::ShellInitHandler { shell }.execute_sync(self);
                Ok(())
            }
            _ => unreachable!("handler dispatch called for another command family"),
        }
    }

    async fn execute_task_handler(&self, command: Command) -> Result<()> {
        match command {
            Command::Task {
                path,
                package,
                name,
                labels,
                environment,
                format,
                materialize_outputs,
                show_cache_path,
                backend,
                tui,
                interactive,
                help,
                skip_dependencies,
                continue_on_error,
                dry_run,
                task_args,
            } => {
                self.run_command(handler::TaskHandler {
                    path,
                    package,
                    name,
                    labels,
                    environment,
                    format,
                    materialize_outputs,
                    show_cache_path,
                    backend,
                    tui,
                    interactive,
                    help,
                    skip_dependencies,
                    continue_on_error,
                    dry_run,
                    task_args,
                })
                .await
            }
            _ => unreachable!("task dispatch called for another command family"),
        }
    }
}
