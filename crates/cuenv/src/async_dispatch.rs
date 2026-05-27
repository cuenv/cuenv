use super::{ensure_command_module_compatibility, run_oci_activate};
use cuenv::cli::{self, CliError, OkEnvelope, OutputFormat};
use cuenv::commands::{self, Command, CommandExecutor};
use tracing::instrument;

#[derive(Clone, Copy)]
struct InfoAsyncRequest<'a> {
    path: Option<&'a str>,
    package: &'a str,
    meta: bool,
}

#[derive(Clone, Copy)]
struct ChangesetAddRequest<'a> {
    path: &'a str,
    summary: Option<&'a str>,
    description: Option<&'a str>,
    packages: &'a [(String, String)],
}

#[derive(Clone, Copy)]
enum EnvelopeField {
    Message,
    Result,
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

/// Execute Web command - starts web server for event streaming
#[instrument(name = "cuenv_execute_web")]
async fn execute_web_command(port: u16, host: String) -> Result<(), CliError> {
    cuenv_events::emit_command_started!("web");

    // Full web server implementation would require adding a web framework dependency.
    cuenv_events::emit_stdout!(format!(
        "Web server would start on http://{}:{}\nThis feature is not yet implemented.",
        host, port
    ));

    cuenv_events::emit_command_completed!("web", true, 0_u64);
    Ok(())
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
            Some(commands::build::execute_build(&options, executor).map_err(CliError::from))
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
            Some(execute_changeset_command(command, json_format))
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

fn execute_changeset_command(command: &Command, json_format: OutputFormat) -> Result<(), CliError> {
    match command {
        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => {
            let request = ChangesetAddRequest {
                path,
                summary: summary.as_deref(),
                description: description.as_deref(),
                packages,
            };
            execute_changeset_add(request, json_format)
        }
        Command::ChangesetStatus { path, json } => {
            let merged_format = OutputFormat::from_json_flag(*json || json_format.is_json());
            execute_changeset_status(path, merged_format)
        }
        Command::ChangesetFromCommits { path, since } => {
            execute_changeset_from_commits(path, since.as_deref(), json_format)
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
            let opts = commands::release::ReleasePrepareOptions {
                path: path.clone(),
                since: since.clone(),
                dry_run: *dry_run,
                branch: branch.clone(),
                no_pr: *no_pr,
            };
            execute_release_prepare(&opts, json_format)
        }
        Command::ReleaseVersion { path, dry_run } => {
            execute_release_version(path, *dry_run, json_format)
        }
        Command::ReleasePublish { path, dry_run } => {
            execute_release_publish(path, *dry_run, json_format)
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

            execute_release_binaries(opts, json_format).await
        }
        _ => unreachable!("release dispatch called for another command family"),
    }
}

fn execute_changeset_add(
    request: ChangesetAddRequest<'_>,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_add(
        request.path,
        request.packages,
        request.summary,
        request.description,
    ) {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Message),
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset add failed: {e}"),
            "Check package names and bump types (major, minor, patch)",
        )),
    }
}

fn execute_changeset_status(path: &str, json_format: OutputFormat) -> Result<(), CliError> {
    match commands::release::execute_changeset_status_with_format(path, json_format) {
        Ok(output) => {
            cuenv_events::println_redacted(&output);
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset status failed: {e}"),
            "Check that the path is valid",
        )),
    }
}

fn execute_changeset_from_commits(
    path: &str,
    since: Option<&str>,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_changeset_from_commits(path, since) {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Message),
        Err(e) => Err(CliError::eval_with_help(
            format!("Changeset from-commits failed: {e}"),
            "Check that the path is a valid git repository",
        )),
    }
}

fn execute_release_prepare(
    opts: &commands::release::ReleasePrepareOptions,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_release_prepare(opts) {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Result),
        Err(e) => Err(CliError::eval_with_help(
            format!("Release prepare failed: {e}"),
            "Check git history and workspace configuration",
        )),
    }
}

fn execute_release_version(
    path: &str,
    dry_run: cuenv_core::DryRun,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_release_version(path, dry_run) {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Result),
        Err(e) => Err(CliError::eval_with_help(
            format!("Release version failed: {e}"),
            "Create changesets first with 'cuenv changeset add'",
        )),
    }
}

fn execute_release_publish(
    path: &str,
    dry_run: cuenv_core::DryRun,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    let format = if json_format.is_json() {
        commands::release::OutputFormat::Json
    } else {
        commands::release::OutputFormat::Human
    };
    match commands::release::execute_release_publish(path, dry_run, format) {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Result),
        Err(e) => Err(CliError::eval_with_help(
            format!("Release publish failed: {e}"),
            "Check that packages are ready for publishing",
        )),
    }
}

async fn execute_release_binaries(
    opts: commands::release::ReleaseBinariesOptions,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    match commands::release::execute_release_binaries(opts).await {
        Ok(output) => print_enveloped_output(&output, json_format, EnvelopeField::Result),
        Err(e) => Err(CliError::eval_with_help(
            format!("Release binaries failed: {e}"),
            "Check that binaries are built and artifacts directory exists",
        )),
    }
}

fn print_enveloped_output(
    output: &str,
    json_format: OutputFormat,
    field: EnvelopeField,
) -> Result<(), CliError> {
    if json_format.is_json() {
        let payload = match field {
            EnvelopeField::Message => serde_json::json!({ "message": output }),
            EnvelopeField::Result => serde_json::json!({ "result": output }),
        };
        let envelope = OkEnvelope::new(payload);
        let json = serde_json::to_string(&envelope)
            .map_err(|e| CliError::other(format!("JSON serialization failed: {e}")))?;
        cuenv_events::println_redacted(&json);
    } else {
        cuenv_events::println_redacted(output);
    }
    Ok(())
}
