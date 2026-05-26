use super::{create_executor, ensure_command_module_compatibility};
use cuenv::cli::{self, CliError, OkEnvelope, OutputFormat};
use cuenv::commands::{self, Command};
use tracing::instrument;

#[derive(Clone, Copy)]
enum JsonEnvelopeField {
    Message,
    Result,
    Script,
    Status,
}

struct InfoSyncRequest {
    path: Option<String>,
    package: String,
    meta: bool,
}

struct EnvStatusSyncRequest {
    path: String,
    package: String,
    format: cli::StatusFormat,
}

struct EnvPrintSyncRequest {
    path: String,
    package: String,
    format: String,
    environment: Option<String>,
}

struct EnvListSyncRequest {
    path: String,
    package: String,
    format: String,
}

struct FmtSyncRequest {
    path: String,
    package: String,
    fix: bool,
    only: Option<Vec<String>>,
}

struct ExportSyncRequest {
    shell: Option<String>,
    path: String,
    package: String,
}

pub fn execute_sync_command(command: Command, json_format: OutputFormat) -> Result<(), CliError> {
    ensure_command_module_compatibility(&command)?;

    match command {
        Command::Version { format: _ } => {
            execute_version_command();
            Ok(())
        }
        Command::Info {
            path,
            package,
            meta,
        } => execute_info_command(
            InfoSyncRequest {
                path,
                package,
                meta,
            },
            json_format,
        ),
        Command::ShellInit { shell } => execute_shell_init_command(shell, json_format),
        command @ (Command::EnvStatus { wait: false, .. }
        | Command::EnvPrint { .. }
        | Command::EnvList { .. }) => execute_env_command(command, json_format),
        command @ (Command::ChangesetAdd { .. }
        | Command::ChangesetStatus { .. }
        | Command::ChangesetFromCommits { .. }) => execute_changeset_command(command, json_format),
        command @ (Command::ReleasePrepare { .. }
        | Command::ReleaseVersion { .. }
        | Command::ReleasePublish { .. }) => execute_release_command(command, json_format),
        Command::Completions { shell } => {
            cli::generate_completions(shell);
            Ok(())
        }
        Command::SecretsSetup { provider, wasm_url } => {
            commands::secrets::execute_secrets_setup(provider, wasm_url.as_deref())
        }
        Command::ToolsList => commands::tools::execute_tools_list(),
        Command::Fmt {
            path,
            package,
            fix,
            only,
        } => execute_fmt_command(FmtSyncRequest {
            path,
            package,
            fix,
            only,
        }),
        Command::Export {
            shell,
            path,
            package,
        } => execute_export_command(ExportSyncRequest {
            shell,
            path,
            package,
        }),
        _ => Err(CliError::other(
            "Internal error: async command reached sync path",
        )),
    }
}

fn execute_version_command() {
    let version_info = commands::version::get_version_info();
    #[allow(clippy::print_stdout)] // Version info contains no secrets
    {
        println!("{version_info}");
    }
}

fn execute_info_command(
    request: InfoSyncRequest,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    let InfoSyncRequest {
        path,
        package,
        meta,
    } = request;
    let options = commands::info::InfoOptions {
        path: path.as_deref(),
        package: &package,
        json_output: json_format.is_json(),
        with_meta: meta,
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

#[instrument(name = "cuenv_execute_shell_init_sync")]
fn execute_shell_init_command(
    shell: cli::ShellType,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    let output = commands::hooks::execute_shell_init(shell);
    print_enveloped_or_text(JsonEnvelopeField::Script, &output, json_format)
}

fn execute_env_command(command: Command, json_format: OutputFormat) -> Result<(), CliError> {
    match command {
        Command::EnvStatus {
            path,
            package,
            wait: false,
            format,
            ..
        } => execute_env_status_command(
            EnvStatusSyncRequest {
                path,
                package,
                format,
            },
            json_format,
        ),
        Command::EnvPrint {
            path,
            package,
            format,
            environment,
        } => execute_env_print_command(EnvPrintSyncRequest {
            path,
            package,
            format,
            environment,
        }),
        Command::EnvList {
            path,
            package,
            format,
        } => execute_env_list_command(EnvListSyncRequest {
            path,
            package,
            format,
        }),
        _ => unreachable!("sync env dispatch called for another command family"),
    }
}

fn execute_env_status_command(
    request: EnvStatusSyncRequest,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    let EnvStatusSyncRequest {
        path,
        package,
        format,
    } = request;
    match commands::hooks::execute_env_status_sync(&path, &package, format) {
        Ok(output) => print_enveloped_or_text(JsonEnvelopeField::Status, &output, json_format),
        Err(e) => Err(CliError::eval_with_help(
            format!("Env status failed: {e}"),
            "Check that your env.cue file exists",
        )),
    }
}

fn execute_env_print_command(request: EnvPrintSyncRequest) -> Result<(), CliError> {
    let EnvPrintSyncRequest {
        path,
        package,
        format,
        environment,
    } = request;
    current_thread_runtime()?.block_on(async {
        let executor = create_executor(&package);
        match commands::env::execute_env_print(&path, &format, environment.as_deref(), &executor)
            .await
        {
            Ok(result) => {
                cuenv_events::println_redacted(&result);
                Ok(())
            }
            Err(e) => {
                let cli_err: CliError = e.into();
                Err(cli_err.with_help("Check your CUE files and package configuration"))
            }
        }
    })
}

fn execute_env_list_command(request: EnvListSyncRequest) -> Result<(), CliError> {
    let EnvListSyncRequest {
        path,
        package,
        format,
    } = request;
    current_thread_runtime()?.block_on(async {
        let executor = create_executor(&package);
        match commands::env::execute_env_list(&path, &format, &executor).await {
            Ok(result) => {
                cuenv_events::println_redacted(&result);
                Ok(())
            }
            Err(e) => {
                let cli_err: CliError = e.into();
                Err(cli_err.with_help("Check your CUE files and package configuration"))
            }
        }
    })
}

fn execute_changeset_command(command: Command, json_format: OutputFormat) -> Result<(), CliError> {
    match command {
        Command::ChangesetAdd {
            path,
            summary,
            description,
            packages,
        } => match commands::release::execute_changeset_add(
            &path,
            &packages,
            summary.as_deref(),
            description.as_deref(),
        ) {
            Ok(output) => print_enveloped_or_text(JsonEnvelopeField::Message, &output, json_format),
            Err(e) => Err(CliError::eval_with_help(
                format!("Changeset add failed: {e}"),
                "Check package names and bump types (major, minor, patch)",
            )),
        },
        Command::ChangesetStatus { path, json } => {
            let merged_format = OutputFormat::from_json_flag(json || json_format.is_json());
            match commands::release::execute_changeset_status_with_format(&path, merged_format) {
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
        Command::ChangesetFromCommits { path, since } => {
            match commands::release::execute_changeset_from_commits(&path, since.as_deref()) {
                Ok(output) => {
                    print_enveloped_or_text(JsonEnvelopeField::Message, &output, json_format)
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Changeset from-commits failed: {e}"),
                    "Check that the path is a valid git repository",
                )),
            }
        }
        _ => unreachable!("sync changeset dispatch called for another command family"),
    }
}

fn execute_release_command(command: Command, json_format: OutputFormat) -> Result<(), CliError> {
    match command {
        Command::ReleasePrepare {
            path,
            since,
            dry_run,
            branch,
            no_pr,
        } => {
            let opts = commands::release::ReleasePrepareOptions {
                path,
                since,
                dry_run,
                branch,
                no_pr,
            };
            match commands::release::execute_release_prepare(&opts) {
                Ok(output) => {
                    print_enveloped_or_text(JsonEnvelopeField::Result, &output, json_format)
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Release prepare failed: {e}"),
                    "Check git history and workspace configuration",
                )),
            }
        }
        Command::ReleaseVersion { path, dry_run } => {
            match commands::release::execute_release_version(&path, dry_run) {
                Ok(output) => {
                    print_enveloped_or_text(JsonEnvelopeField::Result, &output, json_format)
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Release version failed: {e}"),
                    "Create changesets first with 'cuenv changeset add'",
                )),
            }
        }
        Command::ReleasePublish { path, dry_run } => {
            let format = if json_format.is_json() {
                commands::release::OutputFormat::Json
            } else {
                commands::release::OutputFormat::Human
            };
            match commands::release::execute_release_publish(&path, dry_run, format) {
                Ok(output) => {
                    print_enveloped_or_text(JsonEnvelopeField::Result, &output, json_format)
                }
                Err(e) => Err(CliError::eval_with_help(
                    format!("Release publish failed: {e}"),
                    "Check that packages are ready for publishing",
                )),
            }
        }
        _ => unreachable!("sync release dispatch called for another command family"),
    }
}

fn execute_fmt_command(request: FmtSyncRequest) -> Result<(), CliError> {
    let FmtSyncRequest {
        path,
        package,
        fix,
        only,
    } = request;
    match commands::fmt::execute_fmt(&path, &package, fix, only.as_deref()) {
        Ok(output) => {
            cuenv_events::println_redacted(&output);
            Ok(())
        }
        Err(e) => Err(CliError::eval_with_help(
            format!("Format failed: {e}"),
            "Check your formatters configuration in env.cue",
        )),
    }
}

fn execute_export_command(request: ExportSyncRequest) -> Result<(), CliError> {
    let ExportSyncRequest {
        shell,
        path,
        package,
    } = request;
    match commands::export::execute_export_sync(shell.as_deref(), &path, &package) {
        Ok(Some(output)) => {
            cuenv_events::print_redacted(&output);
            Ok(())
        }
        Ok(None) => current_thread_runtime()?.block_on(async {
            match commands::export::execute_export(shell.as_deref(), &path, &package, None).await {
                Ok(result) => {
                    cuenv_events::print_redacted(&result);
                    Ok(())
                }
                Err(e) => {
                    let cli_err: CliError = e.into();
                    Err(cli_err.with_help("Check your CUE configuration"))
                }
            }
        }),
        Err(e) => {
            let cli_err: CliError = e.into();
            Err(cli_err.with_help("Check your CUE configuration"))
        }
    }
}

fn print_enveloped_or_text(
    field: JsonEnvelopeField,
    output: &str,
    json_format: OutputFormat,
) -> Result<(), CliError> {
    if !json_format.is_json() {
        cuenv_events::println_redacted(output);
        return Ok(());
    }

    let payload = match field {
        JsonEnvelopeField::Message => serde_json::json!({ "message": output }),
        JsonEnvelopeField::Result => serde_json::json!({ "result": output }),
        JsonEnvelopeField::Script => serde_json::json!({ "script": output }),
        JsonEnvelopeField::Status => serde_json::json!({ "status": output }),
    };
    let envelope = OkEnvelope::new(payload);
    match serde_json::to_string(&envelope) {
        Ok(json) => {
            cuenv_events::println_redacted(&json);
            Ok(())
        }
        Err(e) => Err(CliError::other(format!("JSON serialization failed: {e}"))),
    }
}

fn current_thread_runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::other(format!("Runtime error: {e}")))
}
