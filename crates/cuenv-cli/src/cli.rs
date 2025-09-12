use crate::commands::Command;
use clap::{Parser, Subcommand, ValueEnum};
use miette::{Diagnostic, Report};
use serde::{Deserialize, Serialize};
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

    #[arg(long, global = true, help = "Output format", value_enum, default_value_t = OutputFormat::Simple)]
    pub format: OutputFormat,

    #[arg(long, global = true, help = "Emit JSON envelope regardless of format")]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Show version information")]
    Version,
    #[command(about = "Environment variable operations")]
    Env {
        #[command(subcommand)]
        subcommand: EnvCommands,
    },
    #[command(
        about = "Execute a task defined in CUE configuration",
        visible_alias = "t"
    )]
    Task {
        #[arg(help = "Name of the task to execute (list tasks if not provided)")]
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
    },
    #[command(
        about = "Execute a command with CUE environment variables",
        visible_alias = "e"
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
        #[arg(long, help = "Optional note about this approval")]
        note: Option<String>,
    },
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
        #[arg(long, help = "Wait for hooks to complete before returning")]
        wait: bool,
        #[arg(long, help = "Timeout in seconds for waiting", default_value = "300")]
        timeout: u64,
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

impl From<Commands> for Command {
    fn from(cmd: Commands) -> Self {
        match cmd {
            Commands::Version => Command::Version,
            Commands::Env { subcommand } => match subcommand {
                EnvCommands::Print {
                    path,
                    package,
                    output_format,
                } => Command::EnvPrint {
                    path,
                    package,
                    format: output_format.to_string(),
                },
                EnvCommands::Load { path } => Command::EnvLoad { path },
                EnvCommands::Status {
                    path,
                    wait,
                    timeout,
                } => Command::EnvStatus {
                    path,
                    wait,
                    timeout,
                },
            },
            Commands::Task {
                name,
                path,
                package,
            } => Command::Task {
                path,
                package,
                name,
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
            },
            Commands::Shell { subcommand } => match subcommand {
                ShellCommands::Init { shell } => Command::ShellInit { shell },
            },
            Commands::Allow { path, note } => Command::Allow { path, note },
        }
    }
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
        assert!(matches!(cli.format, OutputFormat::Simple)); // Default format
        assert!(!cli.json); // Default JSON is false
        assert!(matches!(cli.command, Commands::Version));
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
        let cli = Cli::try_parse_from(["cuenv", "--format", "json", "version"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Json));
    }

    #[test]
    fn test_cli_combined_flags() {
        let cli = Cli::try_parse_from([
            "cuenv", "--level", "debug", "--json", "--format", "env", "version",
        ])
        .unwrap();

        assert!(matches!(cli.level, LogLevel::Debug));
        assert!(cli.json);
        assert!(matches!(cli.format, OutputFormat::Env));
        assert!(matches!(cli.command, Commands::Version));
    }

    #[test]
    fn test_command_conversion() {
        let version_cmd = Commands::Version;
        let command: Command = version_cmd.into();
        assert!(matches!(command, Command::Version));
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
        let command: Command = env_cmd.into();

        if let Command::EnvPrint {
            path,
            package,
            format,
        } = command
        {
            assert_eq!(path, "test");
            assert_eq!(package, "pkg");
            assert_eq!(format, "json");
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
        let cli = Cli::try_parse_from(["cuenv", "--format", "simple", "version"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Simple));

        let cli = Cli::try_parse_from(["cuenv", "--format", "env", "version"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Env));

        let cli = Cli::try_parse_from(["cuenv", "--format", "json", "version"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Json));
    }

    #[test]
    fn test_invalid_output_format() {
        let result = Cli::try_parse_from(["cuenv", "--format", "invalid", "version"]);
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
}
