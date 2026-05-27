use super::{
    ChangesetCommands, Commands, EnvCommands, OciCommands, ReleaseCommands, RuntimeCommands,
    SecretsCommands, ShellCommands, SyncCommands, ToolsCommands,
};
use crate::commands::Command;
use crate::commands::handler::SyncScope;
use crate::commands::sync::SyncMode;

impl Commands {
    /// Convert CLI commands to internal Command representation.
    ///
    /// The environment parameter comes from the global CLI flag.
    #[must_use]
    pub fn into_command(self, environment: Option<String>) -> Command {
        match self {
            command @ (Self::Version { .. }
            | Self::Info { .. }
            | Self::Exec { .. }
            | Self::Fmt { .. }
            | Self::Allow { .. }
            | Self::Deny { .. }
            | Self::Export { .. }
            | Self::Ci(_)
            | Self::Web { .. }
            | Self::Completions { .. }) => command.into_general_command(environment),
            command @ Self::Task { .. } => command.into_task_command(environment),
            command @ (Self::Env { .. }
            | Self::Shell { .. }
            | Self::Changeset { .. }
            | Self::Release { .. }
            | Self::Sync { .. }
            | Self::Secrets { .. }
            | Self::Runtime { .. }
            | Self::Tools { .. }) => command.into_nested_command(environment),
            command @ (Self::Build { .. }
            | Self::Up { .. }
            | Self::Down { .. }
            | Self::Logs { .. }
            | Self::Ps { .. }
            | Self::Restart { .. }) => command.into_service_command(environment),
        }
    }

    fn into_general_command(self, environment: Option<String>) -> Command {
        match self {
            Self::Version { output_format } => Command::Version {
                format: output_format.to_string(),
            },
            Self::Info {
                path,
                package,
                meta,
            } => Command::Info {
                path,
                package,
                meta,
            },
            Self::Exec {
                command,
                args,
                path,
                package,
            } => Command::Exec {
                path,
                package,
                command,
                args,
                environment,
            },
            Self::Fmt {
                path,
                package,
                fix,
                only,
            } => Command::Fmt {
                path,
                package,
                fix,
                only: only.map(|s| s.split(',').map(|x| x.trim().to_string()).collect()),
            },
            Self::Allow {
                path,
                package,
                note,
                yes,
            } => Command::Allow {
                path,
                package,
                note,
                yes,
            },
            Self::Deny { path, package } => Command::Deny { path, package },
            Self::Export {
                shell,
                path,
                package,
            } => Command::Export {
                shell,
                path,
                package,
            },
            Self::Ci(args) => Command::Ci { args },
            Self::Web { port, host } => Command::Web { port, host },
            Self::Completions { shell } => Command::Completions { shell },
            _ => unreachable!("general command conversion called for another command family"),
        }
    }

    fn into_task_command(self, environment: Option<String>) -> Command {
        match self {
            Self::Task {
                name,
                path,
                package,
                labels,
                output_format,
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
            } => Command::Task {
                path,
                package,
                name,
                labels,
                environment,
                format: output_format.map_or(String::new(), |f| f.to_string()),
                materialize_outputs,
                show_cache_path,
                backend,
                tui,
                interactive,
                help,
                skip_dependencies,
                continue_on_error,
                dry_run: dry_run.into(),
                task_args,
            },
            _ => unreachable!("task command conversion called for another command family"),
        }
    }

    fn into_nested_command(self, environment: Option<String>) -> Command {
        match self {
            Self::Env { subcommand } => env_command(subcommand, environment),
            Self::Shell { subcommand } => match subcommand {
                ShellCommands::Init { shell } => Command::ShellInit { shell },
            },
            Self::Changeset { subcommand } => changeset_command(subcommand),
            Self::Release { subcommand } => release_command(subcommand),
            Self::Sync {
                subcommand,
                path,
                package,
                dry_run,
                check,
                all,
            } => sync_command(SyncCommandInput {
                subcommand,
                path,
                package,
                flags: SyncFlags {
                    dry_run,
                    check,
                    all,
                },
            }),
            Self::Secrets { subcommand } => secrets_command(subcommand),
            Self::Runtime { subcommand } => runtime_command(subcommand),
            Self::Tools { subcommand } => tools_command(&subcommand),
            _ => unreachable!("nested command conversion called for another command family"),
        }
    }

    fn into_service_command(self, environment: Option<String>) -> Command {
        match self {
            Self::Build {
                names,
                path,
                package,
                labels,
            } => Command::Build {
                path,
                package,
                names,
                labels,
            },
            Self::Up {
                services,
                path,
                package,
                labels,
            } => Command::Up {
                path,
                package,
                services,
                labels,
                environment,
            },
            Self::Down {
                services,
                path,
                package,
            } => Command::Down {
                path,
                package,
                services,
            },
            Self::Logs {
                services,
                path,
                package,
                follow,
                lines,
            } => Command::Logs {
                path,
                package,
                services,
                follow,
                lines,
            },
            Self::Ps {
                path,
                package,
                output_format,
            } => Command::Ps {
                path,
                package,
                output_format,
            },
            Self::Restart {
                services,
                path,
                package,
            } => Command::Restart {
                path,
                package,
                services,
            },
            _ => unreachable!("service command conversion called for another command family"),
        }
    }
}

fn env_command(subcommand: EnvCommands, environment: Option<String>) -> Command {
    match subcommand {
        EnvCommands::Print {
            path,
            package,
            output_format,
        } => Command::EnvPrint {
            path,
            package,
            format: output_format.to_string(),
            environment,
        },
        EnvCommands::Load { path, package } => Command::EnvLoad { path, package },
        EnvCommands::Status {
            path,
            package,
            wait,
            timeout,
            output_format,
        } => Command::EnvStatus {
            path,
            package,
            wait,
            timeout,
            format: output_format,
        },
        EnvCommands::Inspect { path, package } => Command::EnvInspect { path, package },
        EnvCommands::Check {
            path,
            package,
            shell,
        } => Command::EnvCheck {
            path,
            package,
            shell,
        },
        EnvCommands::List {
            path,
            package,
            output_format,
        } => Command::EnvList {
            path,
            package,
            format: output_format.to_string(),
        },
    }
}

fn changeset_command(subcommand: ChangesetCommands) -> Command {
    match subcommand {
        ChangesetCommands::Add {
            path,
            summary,
            description,
            packages,
        } => Command::ChangesetAdd {
            path,
            summary,
            description,
            packages: parse_changeset_packages(&packages),
        },
        ChangesetCommands::Status { path, json } => Command::ChangesetStatus { path, json },
        ChangesetCommands::FromCommits { path, since } => {
            Command::ChangesetFromCommits { path, since }
        }
    }
}

fn parse_changeset_packages(packages: &[String]) -> Vec<(String, String)> {
    packages
        .iter()
        .filter_map(|package| package.split_once(':'))
        .map(|(name, bump)| (name.to_string(), bump.to_string()))
        .collect()
}

fn release_command(subcommand: ReleaseCommands) -> Command {
    match subcommand {
        ReleaseCommands::Prepare {
            path,
            since,
            dry_run,
            branch,
            no_pr,
        } => Command::ReleasePrepare {
            path,
            since,
            dry_run: dry_run.into(),
            branch,
            no_pr,
        },
        ReleaseCommands::Version { path, dry_run } => Command::ReleaseVersion {
            path,
            dry_run: dry_run.into(),
        },
        ReleaseCommands::Publish { path, dry_run } => Command::ReleasePublish {
            path,
            dry_run: dry_run.into(),
        },
        ReleaseCommands::Binaries {
            path,
            dry_run,
            backend,
            build_only,
            package_only,
            publish_only,
            target,
            version,
        } => Command::ReleaseBinaries {
            path,
            dry_run: dry_run.into(),
            backends: backend,
            build_only,
            package_only,
            publish_only,
            targets: target,
            version,
        },
    }
}

struct SyncCommandInput {
    subcommand: Option<SyncCommands>,
    path: String,
    package: String,
    flags: SyncFlags,
}

#[derive(Clone, Copy)]
struct SyncFlags {
    dry_run: bool,
    check: bool,
    all: bool,
}

impl SyncFlags {
    fn combined(self, subcommand: Self) -> Self {
        Self {
            dry_run: self.dry_run || subcommand.dry_run,
            check: self.check || subcommand.check,
            all: self.all || subcommand.all,
        }
    }

    fn mode(self) -> SyncMode {
        if self.check {
            SyncMode::Check
        } else if self.dry_run {
            SyncMode::DryRun
        } else {
            SyncMode::Write
        }
    }

    fn scope(self) -> SyncScope {
        if self.all {
            SyncScope::Workspace
        } else {
            SyncScope::Path
        }
    }
}

struct SyncResolution {
    provider_name: Option<String>,
    path: String,
    package: String,
    flags: SyncFlags,
    show_diff: bool,
    ci_provider: Option<String>,
    update_tools: Option<Vec<String>>,
}

fn sync_command(input: SyncCommandInput) -> Command {
    let resolution = resolve_sync_command(input);
    Command::Sync {
        subcommand: resolution.provider_name,
        path: resolution.path,
        package: resolution.package,
        mode: resolution.flags.mode(),
        scope: resolution.flags.scope(),
        show_diff: resolution.show_diff,
        ci_provider: resolution.ci_provider,
        update_tools: resolution.update_tools,
    }
}

fn resolve_sync_command(input: SyncCommandInput) -> SyncResolution {
    let SyncCommandInput {
        subcommand,
        path: base_path,
        package: base_package,
        flags,
    } = input;

    match subcommand {
        Some(SyncCommands::Lock {
            path,
            package,
            dry_run,
            check,
            all,
            update,
        }) => sync_provider_resolution(SyncProviderInput {
            provider_name: "lock",
            path: effective_path(&base_path, path),
            package: effective_package(&base_package, package),
            flags: flags.combined(SyncFlags {
                dry_run,
                check,
                all,
            }),
            show_diff: false,
            ci_provider: None,
            update_tools: update.map(filter_update_tools),
        }),
        Some(SyncCommands::Codegen {
            path,
            package,
            dry_run,
            check,
            diff,
            all,
        }) => sync_provider_resolution(SyncProviderInput {
            provider_name: "codegen",
            path: effective_path(&base_path, path),
            package: effective_package(&base_package, package),
            flags: flags.combined(SyncFlags {
                dry_run,
                check,
                all,
            }),
            show_diff: diff,
            ci_provider: None,
            update_tools: None,
        }),
        Some(SyncCommands::Ci {
            path,
            package,
            dry_run,
            check,
            all,
            provider,
        }) => sync_provider_resolution(SyncProviderInput {
            provider_name: "ci",
            path: effective_path(&base_path, path),
            package: effective_package(&base_package, package),
            flags: flags.combined(SyncFlags {
                dry_run,
                check,
                all,
            }),
            show_diff: false,
            ci_provider: provider,
            update_tools: None,
        }),
        Some(SyncCommands::Vcs {
            path,
            package,
            dry_run,
            check,
            all,
            update,
        }) => sync_provider_resolution(SyncProviderInput {
            provider_name: "vcs",
            path: effective_path(&base_path, path),
            package: effective_package(&base_package, package),
            flags: flags.combined(SyncFlags {
                dry_run,
                check,
                all,
            }),
            show_diff: false,
            ci_provider: None,
            update_tools: update.map(filter_update_tools),
        }),
        None => SyncResolution {
            provider_name: None,
            path: base_path,
            package: base_package,
            flags,
            show_diff: false,
            ci_provider: None,
            update_tools: None,
        },
    }
}

fn effective_path(base_path: &str, subcommand_path: String) -> String {
    if subcommand_path == "." {
        base_path.to_string()
    } else {
        subcommand_path
    }
}

fn effective_package(base_package: &str, subcommand_package: String) -> String {
    if subcommand_package == "cuenv" {
        base_package.to_string()
    } else {
        subcommand_package
    }
}

struct SyncProviderInput {
    provider_name: &'static str,
    path: String,
    package: String,
    flags: SyncFlags,
    show_diff: bool,
    ci_provider: Option<String>,
    update_tools: Option<Vec<String>>,
}

fn sync_provider_resolution(input: SyncProviderInput) -> SyncResolution {
    SyncResolution {
        provider_name: Some(input.provider_name.to_string()),
        path: input.path,
        package: input.package,
        flags: input.flags,
        show_diff: input.show_diff,
        ci_provider: input.ci_provider,
        update_tools: input.update_tools,
    }
}

fn filter_update_tools(names: Vec<String>) -> Vec<String> {
    names.into_iter().filter(|name| !name.is_empty()).collect()
}

fn secrets_command(subcommand: SecretsCommands) -> Command {
    match subcommand {
        SecretsCommands::Setup { provider, wasm_url } => {
            Command::SecretsSetup { provider, wasm_url }
        }
    }
}

fn runtime_command(subcommand: RuntimeCommands) -> Command {
    match subcommand {
        RuntimeCommands::Oci { subcommand } => match subcommand {
            OciCommands::Activate => Command::RuntimeOciActivate,
        },
    }
}

fn tools_command(subcommand: &ToolsCommands) -> Command {
    match subcommand {
        ToolsCommands::Download => Command::ToolsDownload,
        ToolsCommands::Activate => Command::ToolsActivate,
        ToolsCommands::List => Command::ToolsList,
    }
}
