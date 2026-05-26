use super::{
    ensure_command_module_compatibility, execute_changeset_add_safe,
    execute_changeset_from_commits_safe, execute_changeset_status_safe,
    execute_release_binaries_safe, execute_release_prepare_safe, execute_release_publish_safe,
    execute_release_version_safe, execute_web_command, run_oci_activate,
};
use cuenv::cli::{self, CliError, OutputFormat};
use cuenv::commands::{self, Command, CommandExecutor};
use tracing::instrument;

#[derive(Clone, Copy)]
struct InfoAsyncRequest<'a> {
    path: Option<&'a str>,
    package: &'a str,
    meta: bool,
}

/// Execute a command using the event-driven async path.
#[instrument(name = "cuenv_execute_command_safe", skip(executor))]
pub async fn execute_command_safe(
    command: Command,
    json_format: OutputFormat,
    executor: &CommandExecutor,
) -> Result<(), CliError> {
    ensure_command_module_compatibility(&command)?;

    if let Some(result) = execute_direct_command(&command).await {
        return result;
    }

    if let Some(result) = execute_service_command(&command, executor).await {
        return result;
    }

    if let Some(result) = execute_output_command(&command, json_format).await {
        return result;
    }

    executor.execute(command).await.map_err(|e| {
        let cli_err: CliError = e.into();
        cli_err.with_help("Run with --help for usage information")
    })
}

async fn execute_direct_command(command: &Command) -> Option<Result<(), CliError>> {
    match command {
        Command::Web { port, host } => Some(
            execute_web_command(*port, host.clone())
                .await
                .map_err(|e| CliError::other(e.to_string())),
        ),
        Command::Completions { shell } => {
            cli::generate_completions(*shell);
            Some(Ok(()))
        }
        Command::SecretsSetup { provider, wasm_url } => Some(
            commands::secrets::execute_secrets_setup(*provider, wasm_url.as_deref()),
        ),
        Command::RuntimeOciActivate => Some(run_oci_activate().await),
        Command::ToolsDownload => Some(commands::tools::execute_tools_download().await),
        Command::ToolsActivate => Some(commands::tools::execute_tools_activate()),
        Command::ToolsList => Some(commands::tools::execute_tools_list()),
        _ => None,
    }
}

async fn execute_service_command(
    command: &Command,
    executor: &CommandExecutor,
) -> Option<Result<(), CliError>> {
    match command {
        Command::Build {
            path,
            package,
            names,
            labels,
        } => {
            let options = commands::build::BuildOptions {
                path: path.clone(),
                package: package.clone(),
                names: names.clone(),
                labels: labels.clone(),
            };
            Some(
                commands::build::execute_build(&options, executor)
                    .map_err(|e| CliError::eval(format!("Build command failed: {e}"))),
            )
        }
        Command::Up {
            path,
            package,
            services,
            labels,
            environment,
        } => {
            let options = commands::up::UpOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
                labels: labels.clone(),
                environment: environment.clone(),
            };
            Some(
                commands::up::execute_up(&options, executor)
                    .await
                    .map_err(|e| CliError::eval(format!("Up command failed: {e}"))),
            )
        }
        Command::Down {
            path,
            package,
            services,
        } => {
            let options = commands::down::DownOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
            };
            Some(
                commands::down::execute_down(&options)
                    .map(|_| ())
                    .map_err(|e| CliError::eval(format!("Down command failed: {e}"))),
            )
        }
        Command::Logs {
            path,
            package,
            services,
            follow,
            lines,
        } => {
            let options = commands::logs::LogsOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
                follow: *follow,
                lines: *lines,
            };
            Some(
                commands::logs::execute_logs(&options)
                    .map(|_| ())
                    .map_err(|e| CliError::eval(format!("Logs command failed: {e}"))),
            )
        }
        Command::Ps {
            path,
            package,
            output_format,
        } => {
            let options = commands::ps::PsOptions {
                path: path.clone(),
                package: package.clone(),
                output_format: output_format.clone(),
            };
            Some(
                commands::ps::execute_ps(&options)
                    .map(|_| ())
                    .map_err(|e| CliError::eval(format!("Ps command failed: {e}"))),
            )
        }
        Command::Restart {
            path,
            package,
            services,
        } => {
            let options = commands::restart::RestartOptions {
                path: path.clone(),
                package: package.clone(),
                services: services.clone(),
            };
            Some(
                commands::restart::execute_restart(&options)
                    .map(|_| ())
                    .map_err(|e| CliError::eval(format!("Restart command failed: {e}"))),
            )
        }
        _ => None,
    }
}

async fn execute_output_command(
    command: &Command,
    json_format: OutputFormat,
) -> Option<Result<(), CliError>> {
    match command {
        Command::Info {
            path,
            package,
            meta,
        } => Some(execute_info_command(
            InfoAsyncRequest {
                path: path.as_deref(),
                package,
                meta: *meta,
            },
            json_format,
        )),
        command @ (Command::ChangesetAdd { .. }
        | Command::ChangesetStatus { .. }
        | Command::ChangesetFromCommits { .. }) => {
            Some(execute_changeset_command(command, json_format).await)
        }
        command @ (Command::ReleasePrepare { .. }
        | Command::ReleaseVersion { .. }
        | Command::ReleasePublish { .. }
        | Command::ReleaseBinaries { .. }) => {
            Some(execute_release_command(command, json_format).await)
        }
        _ => None,
    }
}

fn execute_info_command(
    request: InfoAsyncRequest<'_>,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    let options = commands::info::InfoOptions {
        path: request.path,
        package: request.package,
        json_output: json_format.is_json(),
        with_meta: request.meta,
    };

    match commands::info::execute_info(options) {
        Ok(output) => {
            cuenv_events::print_redacted(&output);
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Info command failed: {e}"),
            "Check that you are in a CUE module with valid env.cue files",
        )),
    }
}

async fn execute_changeset_command(
    command: &Command,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match command {
        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => {
            execute_changeset_add_safe(
                path.clone(),
                summary.clone(),
                description.clone(),
                packages.clone(),
                json_format,
            )
            .await
        }
        Command::ChangesetStatus { path, json } => {
            let merged_format = OutputFormat::from_json_flag(*json || json_format.is_json());
            execute_changeset_status_safe(path.clone(), merged_format).await
        }
        Command::ChangesetFromCommits { path, since } => {
            execute_changeset_from_commits_safe(path.clone(), since.clone(), json_format).await
        }
        _ => unreachable!("changeset dispatch called for another command family"),
    }
}

async fn execute_release_command(
    command: &Command,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match command {
        Command::ReleasePrepare {
            path,
            since,
            dry_run,
            branch,
            no_pr,
        } => {
            execute_release_prepare_safe(
                path.clone(),
                since.clone(),
                *dry_run,
                branch.clone(),
                *no_pr,
                json_format,
            )
            .await
        }
        Command::ReleaseVersion { path, dry_run } => {
            execute_release_version_safe(path.clone(), *dry_run, json_format).await
        }
        Command::ReleasePublish { path, dry_run } => {
            execute_release_publish_safe(path.clone(), *dry_run, json_format).await
        }
        Command::ReleaseBinaries {
            path,
            dry_run,
            backends,
            build_only,
            package_only,
            publish_only,
            targets,
            version,
        } => {
            let phase = if *build_only {
                commands::release::ReleaseBinariesPhase::Build
            } else if *package_only {
                commands::release::ReleaseBinariesPhase::Package
            } else if *publish_only {
                commands::release::ReleaseBinariesPhase::Publish
            } else {
                commands::release::ReleaseBinariesPhase::Full
            };

            let opts = commands::release::ReleaseBinariesOptions::new(path.clone())
                .with_dry_run(*dry_run)
                .with_backends(backends.clone())
                .with_phase(phase)
                .with_targets(targets.clone())
                .with_version(version.clone());

            execute_release_binaries_safe(opts, json_format).await
        }
        _ => unreachable!("release dispatch called for another command family"),
    }
}
