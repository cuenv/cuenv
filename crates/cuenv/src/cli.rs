use crate::commands::Command;
use crate::completions::task_completer;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use miette::{Diagnostic, Report};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use thiserror::Error;

/// Exit codes for the CLI application
pub const EXIT_OK: i32 = 0;
/// CLI or configuration error exit code
pub const EXIT_CLI: i32 = 2;
/// CUE evaluation or FFI error exit code
pub const EXIT_EVAL: i32 = 3;

/// CLI-specific error types with proper exit code mapping
#[derive(Error, Debug, Clone, Diagnostic)]
pub enum CliError {
    /// CLI or configuration error (exit code 2)
    #[error("CLI/configuration error: {message}")]
    #[diagnostic(code(cuenv::cli::config))]
    Config {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
    /// CUE evaluation or FFI error (exit code 3)
    #[error("Evaluation/FFI error: {message}")]
    #[diagnostic(code(cuenv::cli::eval))]
    Eval {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
    /// Other unexpected error (exit code 3)
    #[error("Unexpected error: {message}")]
    #[diagnostic(code(cuenv::cli::other))]
    Other {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
}

impl CliError {
    /// Create a new configuration error
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new configuration error with help text
    #[must_use]
    pub fn config_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    /// Create a new evaluation error
    #[must_use]
    #[allow(dead_code)]
    pub fn eval(message: impl Into<String>) -> Self {
        Self::Eval {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new evaluation error with help text
    #[must_use]
    pub fn eval_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Eval {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    /// Create a new other error
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new other error with help text
    #[must_use]
    #[allow(dead_code)]
    pub fn other_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Other {
            message: message.into(),
            help: Some(help.into()),
        }
    }
}

/// Convert `cuenv_core::Error` to appropriate `CliError` variant.
///
/// Maps error types to their appropriate CLI categories:
/// - Configuration errors (task not found, invalid config) -> Config (exit code 2)
/// - FFI/CUE evaluation errors -> Eval (exit code 3)
/// - I/O and other errors -> Other (exit code 3)
impl From<cuenv_core::Error> for CliError {
    fn from(err: cuenv_core::Error) -> Self {
        match err {
            // Configuration errors are user-facing config issues (exit code 2)
            // Extract just the message to avoid "Configuration error: Configuration error:"
            cuenv_core::Error::Configuration { message, .. } => CliError::config(message),
            // FFI, CUE parsing, validation, and execution errors are evaluation errors (exit code 3)
            cuenv_core::Error::Ffi { .. }
            | cuenv_core::Error::CueParse { .. }
            | cuenv_core::Error::Validation { .. } => CliError::eval(err.to_string()),
            // Execution errors - extract message to avoid redundant prefix
            cuenv_core::Error::Execution { message, .. } => {
                CliError::eval_with_help(message, "Check the task output above for details")
            }
            // I/O, encoding, and timeout errors are unexpected runtime errors
            cuenv_core::Error::Io { .. }
            | cuenv_core::Error::Utf8 { .. }
            | cuenv_core::Error::Timeout { .. } => CliError::other(err.to_string()),
        }
    }
}

/// Map CLI error to appropriate exit code
#[must_use]
pub fn exit_code_for(err: &CliError) -> i32 {
    match err {
        CliError::Config { .. } => EXIT_CLI,
        CliError::Eval { .. } | CliError::Other { .. } => EXIT_EVAL,
    }
}

/// Render error appropriately based on JSON flag
pub fn render_error(err: &CliError, json_mode: bool) {
    if json_mode {
        let error_envelope = ErrorEnvelope::new(serde_json::json!({
            "code": match err {
                CliError::Config { .. } => "config",
                CliError::Eval { .. } => "eval",
                CliError::Other { .. } => "other",
            },
            "message": err.to_string()
        }));

        match serde_json::to_string(&error_envelope) {
            Ok(json) => println!("{json}"),
            Err(_) => eprintln!("Error serializing error response"),
        }
    } else {
        // Use miette for human-friendly error display
        let report = Report::new(err.clone());
        eprintln!("{report:?}");
        // Ensure output is flushed before potential process exit
        let _ = io::stderr().flush();
    }
}

/// Output format for command results
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, ValueEnum, Serialize, Deserialize, Default)]
#[must_use]
pub enum OutputFormat {
    /// JSON output format
    Json,
    /// Environment variable format (KEY=VALUE lines)
    Env,
    /// Simple text format
    #[default]
    Simple,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OutputFormat::Json => "json",
            OutputFormat::Env => "env",
            OutputFormat::Simple => "simple",
        };
        write!(f, "{s}")
    }
}

impl AsRef<str> for OutputFormat {
    fn as_ref(&self) -> &str {
        match self {
            OutputFormat::Json => "json",
            OutputFormat::Env => "env",
            OutputFormat::Simple => "simple",
        }
    }
}

/// Success response envelope for JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkEnvelope<T> {
    /// Status indicator - always "ok" for success
    pub status: &'static str,
    /// The actual data payload
    pub data: T,
}

impl<T> OkEnvelope<T> {
    /// Create a new success envelope
    #[must_use]
    #[allow(dead_code)]
    pub fn new(data: T) -> Self {
        Self { status: "ok", data }
    }
}

/// Error response envelope for JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope<E> {
    /// Status indicator - always "error" for failures
    pub status: &'static str,
    /// The error details
    pub error: E,
}

impl<E> ErrorEnvelope<E> {
    /// Create a new error envelope
    #[must_use]
    pub fn new(error: E) -> Self {
        Self {
            status: "error",
            error,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "cuenv")]
#[command(
    about = "A modern application build toolchain with typed environments and CUE-powered task orchestration"
)]
#[command(long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(
        short = 'l',
        long,
        global = true,
        help = "Set logging level",
        default_value = "warn",
        value_enum
    )]
    pub level: crate::tracing::LogLevel,

    #[arg(long, global = true, help = "Emit JSON envelope regardless of format")]
    pub json: bool,

    #[arg(
        long = "env",
        short = 'e',
        global = true,
        help = "Apply environment-specific overrides (e.g., development, production)"
    )]
    pub environment: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Show version information")]
    Version {
        #[arg(
            long = "output-format",
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Simple
        )]
        output_format: OutputFormat,
    },
    #[command(about = "Environment variable operations")]
    Env {
        #[command(subcommand)]
        subcommand: EnvCommands,
    },
    #[command(
        about = "Execute a task defined in CUE configuration",
        visible_alias = "t",
        disable_help_flag = true,
        trailing_var_arg = true
    )]
    Task {
        #[arg(help = "Name of the task to execute (list tasks if not provided)", add = task_completer())]
        name: Option<String>,
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(
            long = "output-format",
            help = "Output format (only used when listing tasks)",
            value_enum,
            default_value_t = OutputFormat::Simple
        )]
        output_format: OutputFormat,
        #[arg(
            long = "materialize-outputs",
            help = "Materialize cached outputs to this directory on cache hit (off by default)",
            value_name = "DIR"
        )]
        materialize_outputs: Option<String>,
        #[arg(
            long = "show-cache-path",
            help = "Print the cache path for this task key",
            default_value_t = false
        )]
        show_cache_path: bool,
        #[arg(
            long = "backend",
            help = "Force specific execution backend (e.g., 'host', 'dagger')",
            value_name = "BACKEND"
        )]
        backend: Option<String>,
        #[arg(long, action = clap::ArgAction::SetTrue, help = "Print help")]
        help: bool,
        #[arg(help = "Arguments to pass to the task (positional and --named values)")]
        task_args: Vec<String>,
    },
    #[command(
        about = "Execute a command with CUE environment variables",
        visible_alias = "x"
    )]
    Exec {
        #[arg(help = "Command to execute")]
        command: String,
        #[arg(help = "Arguments for the command", trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    #[command(about = "Shell integration commands")]
    Shell {
        #[command(subcommand)]
        subcommand: ShellCommands,
    },
    #[command(about = "Approve configuration for hook execution")]
    Allow {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(long, help = "Optional note about this approval")]
        note: Option<String>,
        #[arg(long, short = 'y', help = "Approve without prompting")]
        yes: bool,
    },
    #[command(about = "Revoke approval for hook execution")]
    Deny {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(
            long,
            help = "Revoke all approvals for this directory (default behavior currently)"
        )]
        all: bool,
    },
    #[command(
        about = "Export environment variables for shell evaluation",
        hide = true
    )]
    Export {
        #[arg(long, short = 's', help = "Shell type (bash, zsh, fish, powershell)")]
        shell: Option<String>,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    #[command(about = "Run CI pipelines")]
    Ci {
        #[arg(long, help = "Show what would be executed without running it")]
        dry_run: bool,
        #[arg(long, help = "Force a specific pipeline to run")]
        pipeline: Option<String>,
        #[arg(long, help = "Generate CI workflow file (e.g., 'github')")]
        generate: Option<String>,
        #[arg(long, help = "Base ref to compare against (branch name or commit SHA)")]
        from: Option<String>,
    },
    #[command(about = "Start interactive TUI dashboard for monitoring cuenv events")]
    Tui,
    #[command(about = "Start web server for streaming cuenv events")]
    Web {
        #[arg(long, short = 'p', help = "Port to listen on", default_value = "3000")]
        port: u16,
        #[arg(long, help = "Host to bind to", default_value = "127.0.0.1")]
        host: String,
    },
    #[command(about = "Manage changesets for release")]
    Changeset {
        #[command(subcommand)]
        subcommand: ChangesetCommands,
    },
    #[command(about = "Release management operations")]
    Release {
        #[command(subcommand)]
        subcommand: ReleaseCommands,
    },
    #[command(about = "Generate shell completions")]
    Completions {
        #[arg(help = "Shell type", value_enum)]
        shell: Shell,
    },
}

/// Output format for status command
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, ValueEnum, Serialize, Deserialize, Default)]
#[must_use]
pub enum StatusFormat {
    /// Default detailed text format
    #[default]
    Text,
    /// Short format (e.g., "[3/5]")
    Short,
    /// Starship module format (JSON)
    Starship,
}

impl std::fmt::Display for StatusFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            StatusFormat::Text => "text",
            StatusFormat::Short => "short",
            StatusFormat::Starship => "starship",
        };
        write!(f, "{s}")
    }
}

#[derive(Subcommand, Debug)]
pub enum EnvCommands {
    #[command(about = "Print environment variables from CUE package")]
    Print {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(
            long = "output-format",
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Env
        )]
        output_format: OutputFormat,
    },
    #[command(about = "Load environment and execute hooks in background")]
    Load {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    #[command(about = "Show hook execution status")]
    Status {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(long, help = "Wait for hooks to complete before returning")]
        wait: bool,
        #[arg(long, help = "Timeout in seconds for waiting", default_value = "300")]
        timeout: u64,
        #[arg(
            long = "output-format",
            help = "Output format",
            value_enum,
            default_value_t = StatusFormat::Text
        )]
        output_format: StatusFormat,
    },
    #[command(about = "Inspect cached hook state for the current config")]
    Inspect {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    #[command(about = "Check hook status and output environment for shell")]
    Check {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(
            long,
            help = "Shell type for export format",
            value_enum,
            default_value_t = ShellType::Bash
        )]
        shell: ShellType,
    },
    #[command(about = "List available environments")]
    List {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(
            long = "output-format",
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Simple
        )]
        output_format: OutputFormat,
    },
}

#[derive(Subcommand, Debug)]
pub enum ShellCommands {
    #[command(about = "Generate shell integration script")]
    Init {
        #[arg(help = "Shell type", value_enum)]
        shell: ShellType,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ShellType {
    Fish,
    Bash,
    Zsh,
}

#[derive(Subcommand, Debug)]
pub enum ChangesetCommands {
    #[command(about = "Add a new changeset")]
    Add {
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        #[arg(long, short = 's', help = "Summary of the change")]
        summary: String,
        #[arg(long, short = 'd', help = "Detailed description of the change")]
        description: Option<String>,
        #[arg(
            long,
            short = 'P',
            help = "Package and bump type (format: package:bump, e.g., my-pkg:minor)",
            value_name = "PACKAGE:BUMP"
        )]
        packages: Vec<String>,
    },
    #[command(about = "Show pending changesets")]
    Status {
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
    },
    #[command(about = "Generate changeset from conventional commits")]
    FromCommits {
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        #[arg(long, short = 's', help = "Tag to start from (default: latest)")]
        since: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReleaseCommands {
    #[command(
        about = "Calculate and apply version bumps from changesets (manifest reading not yet implemented)"
    )]
    Version {
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        #[arg(long, help = "Show what would change without making changes")]
        dry_run: bool,
    },
    #[command(about = "Publish packages in topological order (not yet implemented)")]
    Publish {
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        #[arg(long, help = "Show what would be published without publishing")]
        dry_run: bool,
    },
}

impl Commands {
    /// Convert CLI commands to internal Command representation
    /// The environment parameter comes from the global CLI flag
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn into_command(self, environment: Option<String>) -> Command {
        match self {
            Commands::Version { output_format } => Command::Version {
                format: output_format.to_string(),
            },
            Commands::Env { subcommand } => match subcommand {
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
            },
            Commands::Task {
                name,
                path,
                package,
                output_format,
                materialize_outputs,
                show_cache_path,
                backend,
                help,
                task_args,
            } => Command::Task {
                path,
                package,
                name,
                environment,
                format: output_format.to_string(),
                materialize_outputs,
                show_cache_path,
                backend,
                help,
                task_args,
            },
            Commands::Exec {
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
            Commands::Shell { subcommand } => match subcommand {
                ShellCommands::Init { shell } => Command::ShellInit { shell },
            },
            Commands::Allow {
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
            Commands::Deny { path, package, all } => Command::Deny { path, package, all },
            Commands::Export { shell, package } => Command::Export { shell, package },
            Commands::Ci {
                dry_run,
                pipeline,
                generate,
                from,
            } => Command::Ci {
                dry_run,
                pipeline,
                generate,
                from,
            },
            Commands::Tui => Command::Tui,
            Commands::Web { port, host } => Command::Web { port, host },
            Commands::Changeset { subcommand } => match subcommand {
                ChangesetCommands::Add {
                    path,
                    summary,
                    description,
                    packages,
                } => {
                    // Parse package:bump format
                    let parsed_packages: Vec<(String, String)> = packages
                        .iter()
                        .filter_map(|p| {
                            let parts: Vec<&str> = p.splitn(2, ':').collect();
                            if parts.len() == 2 {
                                Some((parts[0].to_string(), parts[1].to_string()))
                            } else {
                                None
                            }
                        })
                        .collect();
                    Command::ChangesetAdd {
                        path,
                        summary,
                        description,
                        packages: parsed_packages,
                    }
                }
                ChangesetCommands::Status { path } => Command::ChangesetStatus { path },
                ChangesetCommands::FromCommits { path, since } => {
                    Command::ChangesetFromCommits { path, since }
                }
            },
            Commands::Release { subcommand } => match subcommand {
                ReleaseCommands::Version { path, dry_run } => {
                    Command::ReleaseVersion { path, dry_run }
                }
                ReleaseCommands::Publish { path, dry_run } => {
                    Command::ReleasePublish { path, dry_run }
                }
            },
            Commands::Completions { shell } => Command::Completions { shell },
        }
    }
}

/// Generate shell completions using `clap_complete`'s dynamic completion system
///
/// The binary itself handles completion requests via environment variables.
/// This function outputs instructions for the user to set up completions.
pub fn generate_completions(shell: Shell) {
    let shell_name = match shell {
        Shell::Bash => "bash",
        Shell::Fish => "fish",
        Shell::Zsh => "zsh",
        Shell::Elvish => "elvish",
        Shell::PowerShell => "powershell",
        _ => "unknown",
    };

    // Print instructions for the user
    println!("# cuenv shell completions for {shell_name}");
    println!("#");
    println!("# Add the following to your shell config:");
    println!();

    match shell {
        Shell::Bash => {
            println!(r"source <(COMPLETE=bash cuenv)");
        }
        Shell::Zsh => {
            println!(r"source <(COMPLETE=zsh cuenv)");
        }
        Shell::Fish => {
            println!(r"COMPLETE=fish cuenv | source");
        }
        Shell::Elvish => {
            println!(r"eval (E:COMPLETE=elvish cuenv | slurp)");
        }
        Shell::PowerShell => {
            println!(
                r#"$env:COMPLETE = "powershell"; cuenv | Out-String | Invoke-Expression; Remove-Item Env:\COMPLETE"#
            );
        }
        _ => {
            println!("# Shell not supported for dynamic completions");
        }
    }
}

/// Try to handle a completion request. Returns true if this was a completion request.
///
/// Call this early in `main()` - if it returns true, exit immediately.
pub fn try_complete() -> bool {
    use clap_complete::env::CompleteEnv;

    // Check if COMPLETE env var is set - if so, handle completion and return true
    if std::env::var("COMPLETE").is_ok() {
        CompleteEnv::with_factory(Cli::command).complete();
        return true;
    }
    false
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::LogLevel;
    use clap::Parser;

    #[test]
    fn test_cli_default_values() {
        let cli = Cli::try_parse_from(["cuenv", "version"]).unwrap();

        assert!(matches!(cli.level, LogLevel::Warn)); // Default log level
        assert!(!cli.json); // Default JSON is false
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Simple);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_cli_log_level_parsing() {
        // Test each level individually
        let cli = Cli::try_parse_from(["cuenv", "--level", "trace", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Trace));

        let cli = Cli::try_parse_from(["cuenv", "--level", "debug", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Debug));

        let cli = Cli::try_parse_from(["cuenv", "--level", "info", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Info));

        let cli = Cli::try_parse_from(["cuenv", "--level", "warn", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Warn));

        let cli = Cli::try_parse_from(["cuenv", "--level", "error", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Error));

        // Test short form for a few cases
        let cli_short = Cli::try_parse_from(["cuenv", "-l", "debug", "version"]).unwrap();
        assert!(matches!(cli_short.level, LogLevel::Debug));

        let cli_short = Cli::try_parse_from(["cuenv", "-l", "error", "version"]).unwrap();
        assert!(matches!(cli_short.level, LogLevel::Error));
    }

    #[test]
    fn test_cli_json_flag() {
        let cli = Cli::try_parse_from(["cuenv", "--json", "version"]).unwrap();
        assert!(cli.json);

        let cli_no_json = Cli::try_parse_from(["cuenv", "version"]).unwrap();
        assert!(!cli_no_json.json);
    }

    #[test]
    fn test_cli_format_option() {
        let cli = Cli::try_parse_from(["cuenv", "version", "--output-format", "json"]).unwrap();
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Json);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_cli_combined_flags() {
        let cli = Cli::try_parse_from([
            "cuenv",
            "--level",
            "debug",
            "--json",
            "version",
            "--output-format",
            "env",
        ])
        .unwrap();

        assert!(matches!(cli.level, LogLevel::Debug));
        assert!(cli.json);
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Env);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_command_conversion() {
        let version_cmd = Commands::Version {
            output_format: OutputFormat::Simple,
        };
        let command: Command = version_cmd.into_command(None);
        match command {
            Command::Version { format } => assert_eq!(format, "simple"),
            _ => panic!("Expected Command::Version"),
        }
    }

    #[test]
    fn test_invalid_log_level() {
        let result = Cli::try_parse_from(["cuenv", "--level", "invalid", "version"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_subcommand() {
        let result = Cli::try_parse_from(["cuenv"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_help_flag() {
        let result = Cli::try_parse_from(["cuenv", "--help"]);
        // Help flag should cause an error with help message
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.kind() == clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn test_env_print_command_default() {
        let cli = Cli::try_parse_from(["cuenv", "env", "print"]).unwrap();

        if let Commands::Env { subcommand } = cli.command {
            if let EnvCommands::Print {
                path,
                package,
                output_format,
            } = subcommand
            {
                assert_eq!(path, ".");
                assert_eq!(package, "cuenv");
                assert!(matches!(output_format, OutputFormat::Env));
            } else {
                panic!("Expected EnvCommands::Print");
            }
        } else {
            panic!("Expected Env command");
        }
    }

    #[test]
    fn test_env_print_command_with_options() {
        let cli = Cli::try_parse_from([
            "cuenv",
            "env",
            "print",
            "--path",
            "examples/env-basic",
            "--package",
            "examples",
            "--output-format",
            "json",
        ])
        .unwrap();

        if let Commands::Env { subcommand } = cli.command {
            match subcommand {
                EnvCommands::Print {
                    path,
                    package,
                    output_format,
                } => {
                    assert_eq!(path, "examples/env-basic");
                    assert_eq!(package, "examples");
                    assert!(matches!(output_format, OutputFormat::Json));
                }
                _ => panic!("Expected EnvCommands::Print"),
            }
        } else {
            panic!("Expected Env command");
        }
    }

    #[test]
    fn test_env_print_command_short_path() {
        let cli = Cli::try_parse_from(["cuenv", "env", "print", "-p", "test/path"]).unwrap();

        if let Commands::Env { subcommand } = cli.command {
            match subcommand {
                EnvCommands::Print {
                    path,
                    package,
                    output_format,
                } => {
                    assert_eq!(path, "test/path");
                    assert_eq!(package, "cuenv"); // default
                    assert!(matches!(output_format, OutputFormat::Env)); // default
                }
                _ => panic!("Expected EnvCommands::Print"),
            }
        } else {
            panic!("Expected Env command");
        }
    }

    #[test]
    fn test_env_command_conversion() {
        let env_cmd = Commands::Env {
            subcommand: EnvCommands::Print {
                path: "test".to_string(),
                package: "pkg".to_string(),
                output_format: OutputFormat::Json,
            },
        };
        let command: Command = env_cmd.into_command(Some("production".to_string()));

        if let Command::EnvPrint {
            path,
            package,
            format,
            environment,
        } = command
        {
            assert_eq!(path, "test");
            assert_eq!(package, "pkg");
            assert_eq!(format, "json");
            assert_eq!(environment, Some("production".to_string()));
        } else {
            panic!("Expected EnvPrint command");
        }
    }

    #[test]
    fn test_output_format_enum() {
        assert_eq!(OutputFormat::default(), OutputFormat::Simple);

        // Test serialization/deserialization
        let json_fmt = OutputFormat::Json;
        let serialized = serde_json::to_string(&json_fmt).unwrap();
        assert_eq!(serialized, "\"Json\"");

        let deserialized: OutputFormat = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, OutputFormat::Json);
    }

    #[test]
    fn test_ok_envelope() {
        let data = "test data";
        let envelope = OkEnvelope::new(data);

        assert_eq!(envelope.status, "ok");
        assert_eq!(envelope.data, "test data");

        // Test serialization
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"data\":\"test data\""));
    }

    #[test]
    fn test_error_envelope() {
        let error = "test error";
        let envelope = ErrorEnvelope::new(error);

        assert_eq!(envelope.status, "error");
        assert_eq!(envelope.error, "test error");

        // Test serialization
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("\"error\":\"test error\""));
    }

    #[test]
    fn test_output_format_value_enum() {
        // Test that the formats work with clap
        let cli = Cli::try_parse_from(["cuenv", "version", "--output-format", "simple"]).unwrap();
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Simple);
        } else {
            panic!("Expected Version command");
        }

        let cli = Cli::try_parse_from(["cuenv", "version", "--output-format", "env"]).unwrap();
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Env);
        } else {
            panic!("Expected Version command");
        }

        let cli = Cli::try_parse_from(["cuenv", "version", "--output-format", "json"]).unwrap();
        if let Commands::Version { output_format } = cli.command {
            assert_eq!(output_format, OutputFormat::Json);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_invalid_output_format() {
        let result = Cli::try_parse_from(["cuenv", "version", "--output-format", "invalid"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_error_types() {
        let config_err = CliError::config("test config error");
        assert!(matches!(config_err, CliError::Config { .. }));
        assert_eq!(exit_code_for(&config_err), EXIT_CLI);

        let eval_err = CliError::eval("test eval error");
        assert!(matches!(eval_err, CliError::Eval { .. }));
        assert_eq!(exit_code_for(&eval_err), EXIT_EVAL);

        let other_err = CliError::other("test other error");
        assert!(matches!(other_err, CliError::Other { .. }));
        assert_eq!(exit_code_for(&other_err), EXIT_EVAL);
    }

    #[test]
    fn test_cli_error_with_help() {
        let config_err = CliError::config_with_help("config problem", "try this fix");
        if let CliError::Config { message, help } = config_err {
            assert_eq!(message, "config problem");
            assert_eq!(help, Some("try this fix".to_string()));
        } else {
            panic!("Expected Config error");
        }

        let eval_err = CliError::eval_with_help("eval problem", "check your CUE files");
        if let CliError::Eval { message, help } = eval_err {
            assert_eq!(message, "eval problem");
            assert_eq!(help, Some("check your CUE files".to_string()));
        } else {
            panic!("Expected Eval error");
        }
    }

    #[test]
    fn test_exit_codes() {
        assert_eq!(EXIT_OK, 0);
        assert_eq!(EXIT_CLI, 2);
        assert_eq!(EXIT_EVAL, 3);

        // Test exit code mapping
        let config_err = CliError::config("test");
        assert_eq!(exit_code_for(&config_err), 2);

        let eval_err = CliError::eval("test");
        assert_eq!(exit_code_for(&eval_err), 3);

        let other_err = CliError::other("test");
        assert_eq!(exit_code_for(&other_err), 3);
    }

    #[test]
    fn test_error_display() {
        let config_err = CliError::config("test config message");
        let display = format!("{config_err}");
        assert!(display.contains("CLI/configuration error"));
        assert!(display.contains("test config message"));

        let eval_err = CliError::eval("test eval message");
        let display = format!("{eval_err}");
        assert!(display.contains("Evaluation/FFI error"));
        assert!(display.contains("test eval message"));
    }

    #[test]
    fn test_cuenv_core_error_conversion() {
        // Configuration errors should map to Config (exit code 2)
        // and extract just the message (not the full "Configuration error: X")
        let config_err = cuenv_core::Error::configuration("Task 'foo' not found");
        let cli_err: CliError = config_err.into();
        assert!(matches!(cli_err, CliError::Config { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_CLI);
        // Verify we don't have redundant prefix
        let display = format!("{cli_err}");
        assert!(!display.contains("Configuration error: Configuration error"));
        assert!(display.contains("Task 'foo' not found"));

        // FFI errors should map to Eval (exit code 3)
        let ffi_err = cuenv_core::Error::ffi("evaluate", "FFI bridge failed");
        let cli_err: CliError = ffi_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

        // CUE parse errors should map to Eval (exit code 3)
        let cue_err = cuenv_core::Error::cue_parse(std::path::Path::new("/test"), "parse failed");
        let cli_err: CliError = cue_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

        // Validation errors should map to Eval (exit code 3)
        let validation_err = cuenv_core::Error::validation("schema validation failed");
        let cli_err: CliError = validation_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

        // I/O errors should map to Other (exit code 3)
        let io_err = cuenv_core::Error::Io {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
            path: None,
            operation: "read".to_string(),
        };
        let cli_err: CliError = io_err.into();
        assert!(matches!(cli_err, CliError::Other { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

        // Timeout errors should map to Other (exit code 3)
        let timeout_err = cuenv_core::Error::Timeout { seconds: 30 };
        let cli_err: CliError = timeout_err.into();
        assert!(matches!(cli_err, CliError::Other { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

        // Execution errors should map to Eval (exit code 3)
        let exec_err = cuenv_core::Error::execution("Dagger execution failed");
        let cli_err: CliError = exec_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);
        // Verify message extraction
        let display = format!("{cli_err}");
        assert!(display.contains("Dagger execution failed"));
        assert!(!display.contains("Task execution failed: Task execution failed"));
    }
}
