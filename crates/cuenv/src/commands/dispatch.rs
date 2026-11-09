use super::{Command, CommandExecutor, handler};
use cuenv_core::Result;
use handler::{CommandHandler, CommandRunner};

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
            Command::Version { format: _ } => {
                cuenv_events::println_redacted(&super::version::get_version_info());
                Ok(())
            }
            command @ (Command::EnvPrint { .. }
            | Command::EnvRecover { .. }
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
            | Command::ShellInit { .. }
            | Command::ShellSession { .. }
            | Command::ShellRefresh) => self.execute_handler_command(command).await,
            Command::Completions { .. }
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
            | Command::Restart { .. }
            | Command::Infrastructure { .. }
            | Command::InfrastructureProvider { .. } => Ok(()),
        }
    }

    async fn execute_env_command(&self, command: Command) -> Result<()> {
        match command {
            Command::EnvRecover {
                list,
                directory_key,
                instance_hash,
                run_id,
                drained,
            } => {
                let request = super::hooks::HookRecoveryRequest {
                    list,
                    directory_key,
                    instance_hash,
                    run_id,
                    drained,
                };
                let output = super::hooks::execute_env_recover(&request).await?;
                cuenv_events::println_redacted(&output);
                Ok(())
            }
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
                let output = handler::EnvCheckHandler {
                    path,
                    package,
                    shell,
                }
                .execute(self)
                .await?;
                if !output.is_empty() {
                    cuenv_events::print_redacted(&output);
                }
                Ok(())
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
                cuenv_events::println_redacted(&super::hooks::execute_shell_init(shell));
                Ok(())
            }
            Command::ShellSession { current } => {
                let id = if current {
                    super::hooks::required_hook_session()?
                } else {
                    uuid::Uuid::new_v4()
                };
                cuenv_events::println_redacted(&id.to_string());
                Ok(())
            }
            Command::ShellRefresh => Err(cuenv_core::Error::configuration(
                "Shell refresh must run through initialized shell integration",
            )),
            _ => unreachable!("handler dispatch called for another command family"),
        }
    }

    async fn execute_task_handler(&self, command: Command) -> Result<()> {
        let handler = handler::TaskHandler::from_command(command, self)?;
        self.run_command(handler).await
    }
}
