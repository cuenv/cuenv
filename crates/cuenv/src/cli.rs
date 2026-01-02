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

    /// Add help text to an existing error, returning a new error with the help text set.
    #[must_use]
    pub fn with_help(self, help_text: impl Into<String>) -> Self {
        let help = Some(help_text.into());
        match self {
            Self::Config { message, .. } => Self::Config { message, help },
            Self::Eval { message, .. } => Self::Eval { message, help },
            Self::Other { message, .. } => Self::Other { message, help },
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
            cuenv_core::Error::Configuration { message, .. } => Self::config(message),
            // FFI, CUE parsing, validation, and execution errors are evaluation errors (exit code 3)
            cuenv_core::Error::Ffi { .. }
            | cuenv_core::Error::CueParse { .. }
            | cuenv_core::Error::Validation { .. } => Self::eval(err.to_string()),
            // Execution errors - extract message to avoid redundant prefix
            cuenv_core::Error::Execution { message, .. } => {
                Self::eval_with_help(message, "Check the task output above for details")
            }
            // Tool resolution and platform errors are user-facing tool issues
            cuenv_core::Error::ToolResolution { message, help } => {
                if let Some(h) = help {
                    Self::eval_with_help(message, h)
                } else {
                    Self::eval(message)
                }
            }
            cuenv_core::Error::Platform { message } => Self::eval(message),
            // I/O errors - include full context for debugging
            cuenv_core::Error::Io {
                source,
                path,
                operation,
            } => {
                let path_str = path
                    .as_ref()
                    .map_or(String::new(), |p| format!(" on {}", p.display()));
                Self::other_with_help(
                    format!("I/O {operation} failed{path_str}: {source}"),
                    "Check file permissions and ensure the path exists",
                )
            }
            // Encoding and timeout errors are unexpected runtime errors
            cuenv_core::Error::Utf8 { .. } | cuenv_core::Error::Timeout { .. } => {
                Self::other(err.to_string())
            }
        }
    }
}

/// Map CLI error to appropriate exit code
#[must_use]
pub const fn exit_code_for(err: &CliError) -> i32 {
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
    /// Plain text format (no colors or styling)
    #[default]
    Text,
    /// Rich styled output with colors and formatting
    Rich,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Json => "json",
            Self::Env => "env",
            Self::Text => "text",
            Self::Rich => "rich",
        };
        write!(f, "{s}")
    }
}

impl AsRef<str> for OutputFormat {
    fn as_ref(&self) -> &str {
        match self {
            Self::Json => "json",
            Self::Env => "env",
            Self::Text => "text",
            Self::Rich => "rich",
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
    pub const fn new(data: T) -> Self {
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
    pub const fn new(error: E) -> Self {
        Self {
            status: "error",
            error,
        }
    }
}

/// Main CLI entry point for cuenv.
///
/// A modern application build toolchain with typed environments and CUE-powered task orchestration.
#[derive(Parser, Debug)]
#[command(name = "cuenv")]
#[command(
    about = "A modern application build toolchain with typed environments and CUE-powered task orchestration"
)]
#[command(long_about = None)]
#[command(version)]
pub struct Cli {
    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Logging verbosity level.
    #[arg(
        short = 'L',
        long,
        global = true,
        help = "Set logging level",
        default_value = "warn",
        value_enum
    )]
    pub level: crate::tracing::LogLevel,

    /// Emit JSON envelope regardless of format.
    #[arg(long, global = true, help = "Emit JSON envelope regardless of format")]
    pub json: bool,

    /// Environment-specific overrides (e.g., development, production).
    #[arg(
        long = "env",
        short = 'e',
        global = true,
        help = "Apply environment-specific overrides (e.g., development, production)"
    )]
    pub environment: Option<String>,

    /// Print LLM context information (llms.txt).
    #[arg(long, global = true, help = "Print LLM context information (llms.txt)")]
    pub llms: bool,
}

/// Available CLI subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show version information.
    #[command(about = "Show version information")]
    Version {
        /// Output format for version information.
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Text
        )]
        output_format: OutputFormat,
    },
    /// Show module information (bases, projects).
    #[command(about = "Show module information (bases, projects)")]
    Info {
        /// Path to a specific directory to evaluate. If omitted, evaluates the entire module recursively.
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Include _meta source location for all values (JSON output).
        #[arg(
            long,
            help = "Include _meta source location for all values (JSON output)"
        )]
        meta: bool,
    },
    /// Environment variable operations.
    #[command(about = "Environment variable operations")]
    Env {
        /// Environment subcommand to execute.
        #[command(subcommand)]
        subcommand: EnvCommands,
    },
    /// Execute a task defined in CUE configuration.
    #[command(
        about = "Execute a task defined in CUE configuration",
        visible_alias = "t",
        disable_help_flag = true,
        trailing_var_arg = true
    )]
    Task {
        /// Name of the task to execute (list tasks if not provided).
        #[arg(help = "Name of the task to execute (list tasks if not provided)", add = task_completer())]
        name: Option<String>,
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Execute all tasks matching the given label (repeatable).
        #[arg(
            long = "label",
            short = 'l',
            action = clap::ArgAction::Append,
            help = "Execute all tasks matching the given label (repeatable)",
            value_name = "LABEL"
        )]
        labels: Vec<String>,
        /// Output format (only used when listing tasks).
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format (only used when listing tasks)",
            value_enum,
            default_value_t = OutputFormat::Text
        )]
        output_format: OutputFormat,
        /// Materialize cached outputs to this directory on cache hit.
        #[arg(
            long = "materialize-outputs",
            help = "Materialize cached outputs to this directory on cache hit (off by default)",
            value_name = "DIR"
        )]
        materialize_outputs: Option<String>,
        /// Print the cache path for this task key.
        #[arg(
            long = "show-cache-path",
            help = "Print the cache path for this task key",
            default_value_t = false
        )]
        show_cache_path: bool,
        /// Force specific execution backend (e.g., 'host', 'dagger').
        #[arg(
            long = "backend",
            help = "Force specific execution backend (e.g., 'host', 'dagger')",
            value_name = "BACKEND"
        )]
        backend: Option<String>,
        /// Use rich TUI for task execution.
        #[arg(long, help = "Use rich TUI for task execution")]
        tui: bool,
        /// Interactive task picker - select a task to run.
        #[arg(
            long,
            short = 'i',
            help = "Interactive task picker - select a task to run"
        )]
        interactive: bool,
        /// Print help.
        #[arg(long, action = clap::ArgAction::SetTrue, help = "Print help")]
        help: bool,
        /// List tasks from all projects in the workspace (for IDE completions).
        #[arg(
            long = "all",
            short = 'A',
            help = "List tasks from all projects in the workspace (for IDE completions)",
            default_value_t = false
        )]
        all: bool,
        /// Skip executing task dependencies (for CI orchestrators that handle deps externally).
        #[arg(
            long = "skip-dependencies",
            short = 'S',
            help = "Skip executing task dependencies (for CI orchestrators that handle deps externally)",
            default_value_t = false
        )]
        skip_dependencies: bool,
        /// Arguments to pass to the task (positional and --named values).
        #[arg(help = "Arguments to pass to the task (positional and --named values)")]
        task_args: Vec<String>,
    },
    /// Execute a command with CUE environment variables.
    #[command(
        about = "Execute a command with CUE environment variables",
        visible_alias = "x"
    )]
    Exec {
        /// Command to execute.
        #[arg(help = "Command to execute")]
        command: String,
        /// Arguments for the command.
        #[arg(help = "Arguments for the command", trailing_var_arg = true)]
        args: Vec<String>,
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    /// Format code based on formatters configuration.
    #[command(about = "Format code based on formatters configuration")]
    Fmt {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Apply formatting changes (default is check mode).
        #[arg(long, help = "Apply formatting changes (default is check mode)")]
        fix: bool,
        /// Run only specific formatters (comma-separated: rust,nix,go,cue).
        #[arg(
            long,
            help = "Run only specific formatters (comma-separated: rust,nix,go,cue)"
        )]
        only: Option<String>,
    },
    /// Shell integration commands.
    #[command(about = "Shell integration commands")]
    Shell {
        /// Shell subcommand to execute.
        #[command(subcommand)]
        subcommand: ShellCommands,
    },
    /// Approve configuration for hook execution.
    #[command(about = "Approve configuration for hook execution")]
    Allow {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Optional note about this approval.
        #[arg(long, help = "Optional note about this approval")]
        note: Option<String>,
        /// Approve without prompting.
        #[arg(long, short = 'y', help = "Approve without prompting")]
        yes: bool,
    },
    /// Revoke approval for hook execution.
    #[command(about = "Revoke approval for hook execution")]
    Deny {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Revoke all approvals for this directory.
        #[arg(
            long,
            help = "Revoke all approvals for this directory (default behavior currently)"
        )]
        all: bool,
    },
    /// Export environment variables for shell evaluation.
    #[command(
        about = "Export environment variables for shell evaluation",
        hide = true
    )]
    Export {
        /// Shell type (bash, zsh, fish, powershell).
        #[arg(long, short = 's', help = "Shell type (bash, zsh, fish, powershell)")]
        shell: Option<String>,
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    /// Run CI pipelines.
    #[command(about = "Run CI pipelines")]
    Ci {
        /// Show what would be executed without running it.
        #[arg(long, help = "Show what would be executed without running it")]
        dry_run: bool,
        /// Force a specific pipeline to run.
        #[arg(long, help = "Force a specific pipeline to run")]
        pipeline: Option<String>,
        /// Output dynamic pipeline YAML to stdout.
        #[arg(
            long,
            help = "Output dynamic pipeline YAML to stdout (e.g., 'buildkite' for buildkite-agent pipeline upload)"
        )]
        dynamic: Option<String>,
        /// Base ref to compare against (branch name or commit SHA).
        #[arg(long, help = "Base ref to compare against (branch name or commit SHA)")]
        from: Option<String>,
    },
    /// Start interactive TUI dashboard for monitoring cuenv events.
    #[command(about = "Start interactive TUI dashboard for monitoring cuenv events")]
    Tui,
    /// Start web server for streaming cuenv events.
    #[command(about = "Start web server for streaming cuenv events")]
    Web {
        /// Port to listen on.
        #[arg(long, short = 'p', help = "Port to listen on", default_value = "3000")]
        port: u16,
        /// Host to bind to.
        #[arg(long, help = "Host to bind to", default_value = "127.0.0.1")]
        host: String,
    },
    /// Manage changesets for release.
    #[command(about = "Manage changesets for release")]
    Changeset {
        /// Changeset subcommand to execute.
        #[command(subcommand)]
        subcommand: ChangesetCommands,
    },
    /// Release management operations.
    #[command(about = "Release management operations")]
    Release {
        /// Release subcommand to execute.
        #[command(subcommand)]
        subcommand: ReleaseCommands,
    },
    /// Generate shell completions.
    #[command(about = "Generate shell completions")]
    Completions {
        /// Shell type to generate completions for.
        #[arg(help = "Shell type", value_enum)]
        shell: Shell,
    },
    /// Sync generated files from CUE configuration.
    #[command(about = "Sync generated files from CUE configuration")]
    Sync {
        /// Sync subcommand to execute.
        #[command(subcommand)]
        subcommand: Option<SyncCommands>,
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Show what would be generated without writing files.
        #[arg(long, help = "Show what would be generated without writing files")]
        dry_run: bool,
        /// Check if files are in sync without making changes.
        #[arg(long, help = "Check if files are in sync without making changes")]
        check: bool,
        /// Sync all projects in the workspace.
        #[arg(long = "all", short = 'A', help = "Sync all projects in the workspace")]
        all: bool,
    },
    /// Secret provider management.
    #[command(about = "Secret provider management")]
    Secrets {
        /// Secrets subcommand to execute.
        #[command(subcommand)]
        subcommand: SecretsCommands,
    },
    /// Runtime management commands.
    #[command(about = "Runtime management commands")]
    Runtime {
        /// Runtime subcommand to execute.
        #[command(subcommand)]
        subcommand: RuntimeCommands,
    },
    /// Multi-source tool management.
    #[command(about = "Multi-source tool management (GitHub, OCI, Nix)")]
    Tools {
        /// Tools subcommand to execute.
        #[command(subcommand)]
        subcommand: ToolsCommands,
    },
}

/// Sync subcommands for generating different types of files.
#[derive(Subcommand, Debug, Clone)]
pub enum SyncCommands {
    /// Resolve OCI images and update lockfile.
    #[command(about = "Resolve OCI images and update lockfile")]
    Lock {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Show what would be resolved without writing lockfile.
        #[arg(long, help = "Show what would be resolved without writing lockfile")]
        dry_run: bool,
        /// Check if lockfile is up-to-date.
        #[arg(long, help = "Check if lockfile is up-to-date")]
        check: bool,
        /// Sync lock for all projects in the workspace.
        #[arg(
            long = "all",
            short = 'A',
            help = "Sync lock for all projects in the workspace"
        )]
        all: bool,
        /// Force re-resolution of tools, ignoring cached lockfile resolutions.
        /// Use without arguments to update all tools, or specify tool names.
        #[arg(
            long = "update",
            short = 'u',
            help = "Force re-resolution of tools. Use -u to update all, or -u bun jq to update specific tools.",
            num_args = 0..,
            value_name = "TOOLS",
            default_missing_value = ""
        )]
        update: Option<Vec<String>>,
    },
    /// Sync files from CUE cube configurations in projects.
    #[command(about = "Sync files from CUE cube configurations in projects")]
    Cubes {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Show what would be generated without writing files.
        #[arg(long, help = "Show what would be generated without writing files")]
        dry_run: bool,
        /// Check if files are in sync without making changes.
        #[arg(long, help = "Check if files are in sync without making changes")]
        check: bool,
        /// Show diff for files that would change.
        #[arg(long, help = "Show diff for files that would change")]
        diff: bool,
        /// Sync cubes for all projects in the workspace.
        #[arg(
            long = "all",
            short = 'A',
            help = "Sync cubes for all projects in the workspace"
        )]
        all: bool,
    },
    /// Sync CI workflow files from CUE configuration.
    #[command(about = "Sync CI workflow files from CUE configuration")]
    Ci {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Show what would be generated without writing files.
        #[arg(long, help = "Show what would be generated without writing files")]
        dry_run: bool,
        /// Check if CI workflows are in sync without making changes.
        #[arg(
            long,
            help = "Check if CI workflows are in sync without making changes"
        )]
        check: bool,
        /// Sync CI workflows for all projects in the workspace.
        #[arg(
            long = "all",
            short = 'A',
            help = "Sync CI workflows for all projects in the workspace"
        )]
        all: bool,
        /// Filter to specific provider (github, buildkite).
        #[arg(long, help = "Filter to specific provider (github, buildkite)")]
        provider: Option<String>,
    },
}

/// Secrets subcommands for managing secret providers.
#[derive(Subcommand, Debug, Clone)]
pub enum SecretsCommands {
    /// Set up a secret provider (download required components).
    #[command(about = "Set up a secret provider (download required components)")]
    Setup {
        /// Provider to set up.
        #[arg(help = "Provider to set up", value_enum)]
        provider: SecretsProvider,
        /// Override the default WASM URL (for 1Password).
        #[arg(
            long,
            help = "Override the default WASM URL (for 1Password)",
            value_name = "URL"
        )]
        wasm_url: Option<String>,
    },
}

/// Supported secret providers that require setup.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum SecretsProvider {
    /// 1Password (downloads WASM SDK for HTTP mode).
    Onepassword,
}

/// Runtime subcommands for managing runtime environments.
#[derive(Subcommand, Debug, Clone)]
pub enum RuntimeCommands {
    /// OCI runtime management.
    #[command(about = "OCI runtime management")]
    Oci {
        /// OCI subcommand to execute.
        #[command(subcommand)]
        subcommand: OciCommands,
    },
}

/// OCI runtime subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum OciCommands {
    /// Activate OCI binaries for the current environment.
    #[command(about = "Activate OCI binaries for the current environment")]
    Activate,
}

/// Tools subcommands for multi-source tool management.
#[derive(Subcommand, Debug, Clone)]
pub enum ToolsCommands {
    /// Download tools for the current platform.
    #[command(about = "Download tools for the current platform from lockfile")]
    Download,
    /// Activate tools (output shell exports for PATH).
    #[command(about = "Activate tools (output shell exports for PATH)")]
    Activate,
    /// List configured tools.
    #[command(about = "List configured tools from lockfile")]
    List,
}

/// Output format for status command.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, ValueEnum, Serialize, Deserialize, Default)]
#[must_use]
pub enum StatusFormat {
    /// Default detailed text format.
    #[default]
    Text,
    /// Short format (e.g., "[3/5]").
    Short,
    /// Starship module format (JSON).
    Starship,
}

impl std::fmt::Display for StatusFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Text => "text",
            Self::Short => "short",
            Self::Starship => "starship",
        };
        write!(f, "{s}")
    }
}

/// Environment variable subcommands.
#[derive(Subcommand, Debug)]
pub enum EnvCommands {
    /// Print environment variables from CUE package.
    #[command(about = "Print environment variables from CUE package")]
    Print {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Output format for environment variables.
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Env
        )]
        output_format: OutputFormat,
    },
    /// Load environment and execute hooks in background.
    #[command(about = "Load environment and execute hooks in background")]
    Load {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    /// Show hook execution status.
    #[command(about = "Show hook execution status")]
    Status {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Wait for hooks to complete before returning.
        #[arg(long, help = "Wait for hooks to complete before returning")]
        wait: bool,
        /// Timeout in seconds for waiting.
        #[arg(long, help = "Timeout in seconds for waiting", default_value = "300")]
        timeout: u64,
        /// Output format for status information.
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format",
            value_enum,
            default_value_t = StatusFormat::Text
        )]
        output_format: StatusFormat,
    },
    /// Inspect cached hook state for the current config.
    #[command(about = "Inspect cached hook state for the current config")]
    Inspect {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
    },
    /// Check hook status and output environment for shell.
    #[command(about = "Check hook status and output environment for shell")]
    Check {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Shell type for export format.
        #[arg(
            long,
            help = "Shell type for export format",
            value_enum,
            default_value_t = ShellType::Bash
        )]
        shell: ShellType,
    },
    /// List available environments.
    #[command(about = "List available environments")]
    List {
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        /// Output format for the environment list.
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format",
            value_enum,
            default_value_t = OutputFormat::Text
        )]
        output_format: OutputFormat,
    },
}

/// Shell integration subcommands.
#[derive(Subcommand, Debug)]
pub enum ShellCommands {
    /// Generate shell integration script.
    #[command(about = "Generate shell integration script")]
    Init {
        /// Shell type to generate integration for.
        #[arg(help = "Shell type", value_enum)]
        shell: ShellType,
    },
}

/// Supported shell types for integration.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ShellType {
    /// Fish shell.
    Fish,
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
}

/// Changeset management subcommands.
#[derive(Subcommand, Debug)]
pub enum ChangesetCommands {
    /// Add a new changeset (interactive if no args provided).
    #[command(about = "Add a new changeset (interactive if no args provided)")]
    Add {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Summary of the change (interactive if omitted).
        #[arg(
            long,
            short = 's',
            help = "Summary of the change (interactive if omitted)"
        )]
        summary: Option<String>,
        /// Detailed description of the change.
        #[arg(long, short = 'd', help = "Detailed description of the change")]
        description: Option<String>,
        /// Package and bump type (format: package:bump).
        #[arg(
            long,
            short = 'P',
            help = "Package and bump type (format: package:bump, e.g., my-pkg:minor). Interactive if omitted.",
            value_name = "PACKAGE:BUMP"
        )]
        packages: Vec<String>,
    },
    /// Show pending changesets.
    #[command(about = "Show pending changesets")]
    Status {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Output in JSON format for CI consumption.
        #[arg(long, help = "Output in JSON format for CI consumption")]
        json: bool,
    },
    /// Generate changeset from conventional commits.
    #[command(about = "Generate changeset from conventional commits")]
    FromCommits {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Tag to start from (default: latest).
        #[arg(long, short = 's', help = "Tag to start from (default: latest)")]
        since: Option<String>,
    },
}

/// Release management subcommands.
#[derive(Subcommand, Debug)]
pub enum ReleaseCommands {
    /// Prepare a release: analyze commits, bump versions, generate changelog, create PR.
    #[command(
        about = "Prepare a release: analyze commits, bump versions, generate changelog, create PR"
    )]
    Prepare {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Git tag or ref to analyze commits from.
        #[arg(long, short = 's', help = "Git tag or ref to analyze commits from")]
        since: Option<String>,
        /// Preview changes without applying.
        #[arg(long, help = "Preview changes without applying")]
        dry_run: bool,
        /// Branch name for the release.
        #[arg(
            long,
            default_value = "release/next",
            help = "Branch name for the release"
        )]
        branch: String,
        /// Skip creating the pull request.
        #[arg(long, help = "Skip creating the pull request")]
        no_pr: bool,
    },
    /// Calculate and apply version bumps from changesets.
    #[command(
        about = "Calculate and apply version bumps from changesets (manifest reading not yet implemented)"
    )]
    Version {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Show what would change without making changes.
        #[arg(long, help = "Show what would change without making changes")]
        dry_run: bool,
    },
    /// Publish workspace packages to crates.io in dependency order.
    #[command(about = "Publish workspace packages to crates.io in dependency order")]
    Publish {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Show what would be published without publishing.
        #[arg(long, help = "Show what would be published without publishing")]
        dry_run: bool,
    },
    /// Build, package, and publish binary releases to configured backends.
    #[command(about = "Build, package, and publish binary releases to configured backends")]
    Binaries {
        /// Path to project root.
        #[arg(long, short = 'p', help = "Path to project root", default_value = ".")]
        path: String,
        /// Preview without making changes.
        #[arg(long, help = "Preview without making changes")]
        dry_run: bool,
        /// Only run specific backend(s).
        #[arg(long, help = "Only run specific backend(s)", value_delimiter = ',')]
        backend: Option<Vec<String>>,
        /// Build only, don't publish.
        #[arg(long, help = "Build only, don't publish")]
        build_only: bool,
        /// Package only, don't publish (assumes binaries exist).
        #[arg(long, help = "Package only, don't publish (assumes binaries exist)")]
        package_only: bool,
        /// Publish only (requires existing artifacts).
        #[arg(long, help = "Publish only (requires existing artifacts)")]
        publish_only: bool,
        /// Target platform(s) to build.
        #[arg(long, help = "Target platform(s) to build", value_delimiter = ',')]
        target: Option<Vec<String>>,
        /// Version to release (default: from Cargo.toml).
        #[arg(long, help = "Version to release (default: from Cargo.toml)")]
        version: Option<String>,
    },
}

impl Commands {
    /// Extract the package name from the command.
    ///
    /// This allows accessing the package before consuming the command via `into_command()`.
    /// Used by the [`CommandExecutor`] to cache module evaluation with the correct package.
    /// Commands without CUE evaluation needs return "cuenv" as a reasonable default.
    #[must_use]
    pub fn package(&self) -> &str {
        match self {
            // Commands with explicit --package parameter
            Self::Info { package, .. }
            | Self::Task { package, .. }
            | Self::Exec { package, .. }
            | Self::Fmt { package, .. }
            | Self::Sync { package, .. }
            | Self::Allow { package, .. }
            | Self::Deny { package, .. }
            | Self::Export { package, .. } => package,

            // Nested env subcommands with --package
            Self::Env { subcommand } => match subcommand {
                EnvCommands::Print { package, .. }
                | EnvCommands::Load { package, .. }
                | EnvCommands::Status { package, .. }
                | EnvCommands::Inspect { package, .. }
                | EnvCommands::Check { package, .. }
                | EnvCommands::List { package, .. } => package,
            },

            // Commands that don't use CUE evaluation or have no package param
            Self::Version { .. }
            | Self::Completions { .. }
            | Self::Shell { .. }
            | Self::Ci { .. }
            | Self::Tui
            | Self::Web { .. }
            | Self::Changeset { .. }
            | Self::Release { .. }
            | Self::Secrets { .. }
            | Self::Runtime { .. }
            | Self::Tools { .. } => "cuenv",
        }
    }

    /// Convert CLI commands to internal Command representation
    /// The environment parameter comes from the global CLI flag
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn into_command(self, environment: Option<String>) -> Command {
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
            Self::Env { subcommand } => match subcommand {
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
                all,
                skip_dependencies,
                task_args,
            } => Command::Task {
                path,
                package,
                name,
                labels,
                environment,
                format: output_format.to_string(),
                materialize_outputs,
                show_cache_path,
                backend,
                tui,
                interactive,
                help,
                all,
                skip_dependencies,
                task_args,
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
            Self::Shell { subcommand } => match subcommand {
                ShellCommands::Init { shell } => Command::ShellInit { shell },
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
            Self::Deny { path, package, all } => Command::Deny { path, package, all },
            Self::Export {
                shell,
                path,
                package,
            } => Command::Export {
                shell,
                path,
                package,
            },
            Self::Ci {
                dry_run,
                pipeline,
                dynamic,
                from,
            } => Command::Ci {
                dry_run,
                pipeline,
                dynamic,
                from,
            },
            Self::Tui => Command::Tui,
            Self::Web { port, host } => Command::Web { port, host },
            Self::Changeset { subcommand } => match subcommand {
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
                ChangesetCommands::Status { path, json } => Command::ChangesetStatus { path, json },
                ChangesetCommands::FromCommits { path, since } => {
                    Command::ChangesetFromCommits { path, since }
                }
            },
            Self::Release { subcommand } => match subcommand {
                ReleaseCommands::Prepare {
                    path,
                    since,
                    dry_run,
                    branch,
                    no_pr,
                } => Command::ReleasePrepare {
                    path,
                    since,
                    dry_run,
                    branch,
                    no_pr,
                },
                ReleaseCommands::Version { path, dry_run } => {
                    Command::ReleaseVersion { path, dry_run }
                }
                ReleaseCommands::Publish { path, dry_run } => {
                    Command::ReleasePublish { path, dry_run }
                }
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
                    dry_run,
                    backends: backend,
                    build_only,
                    package_only,
                    publish_only,
                    targets: target,
                    version,
                },
            },
            Self::Completions { shell } => Command::Completions { shell },
            Self::Sync {
                subcommand,
                path,
                package,
                dry_run,
                check,
                all,
            } => {
                use crate::commands::handler::SyncScope;
                use crate::commands::sync::SyncMode;

                // Helper to convert bools to SyncMode
                let to_mode = |dry_run: bool, check: bool| {
                    if dry_run {
                        SyncMode::DryRun
                    } else if check {
                        SyncMode::Check
                    } else {
                        SyncMode::Write
                    }
                };

                // Helper to convert bool to SyncScope
                let to_scope = |all: bool| {
                    if all {
                        SyncScope::Workspace
                    } else {
                        SyncScope::Path
                    }
                };

                // Convert subcommand to provider name and extract provider-specific args
                let (
                    provider_name,
                    effective_path,
                    effective_package,
                    mode,
                    scope,
                    show_diff,
                    ci_provider,
                    update_tools,
                ) = match subcommand {
                    Some(SyncCommands::Lock {
                        path: sub_path,
                        package: sub_package,
                        dry_run: sub_dry_run,
                        check: sub_check,
                        all: sub_all,
                        update: sub_update,
                    }) => {
                        // Convert update flag:
                        // - None: flag not provided, use cache
                        // - Some(vec![""]) or Some(vec![]): -u alone, update all tools
                        // - Some(vec!["bun", "jq"]): update specific tools
                        let update_tools = sub_update
                            .map(|tools| tools.into_iter().filter(|t| !t.is_empty()).collect());
                        (
                            Some("lock".to_string()),
                            sub_path,
                            sub_package,
                            to_mode(sub_dry_run, sub_check),
                            to_scope(sub_all || all),
                            false,
                            None,
                            update_tools,
                        )
                    }
                    Some(SyncCommands::Cubes {
                        path: sub_path,
                        package: sub_package,
                        dry_run: sub_dry_run,
                        check: sub_check,
                        diff: cubes_diff,
                        all: sub_all,
                    }) => (
                        Some("cubes".to_string()),
                        sub_path,
                        sub_package,
                        to_mode(sub_dry_run, sub_check),
                        to_scope(sub_all || all),
                        cubes_diff,
                        None,
                        None, // update_tools only applies to lock
                    ),
                    Some(SyncCommands::Ci {
                        path: sub_path,
                        package: sub_package,
                        dry_run: sub_dry_run,
                        check: sub_check,
                        all: sub_all,
                        provider,
                    }) => (
                        Some("ci".to_string()),
                        sub_path,
                        sub_package,
                        to_mode(sub_dry_run, sub_check),
                        to_scope(sub_all || all),
                        false,
                        provider,
                        None, // update_tools only applies to lock
                    ),
                    None => (
                        None,
                        path,
                        package,
                        to_mode(dry_run, check),
                        to_scope(all),
                        false,
                        None,
                        None, // update_tools only applies to lock
                    ),
                };
                Command::Sync {
                    subcommand: provider_name,
                    path: effective_path,
                    package: effective_package,
                    mode,
                    scope,
                    show_diff,
                    ci_provider,
                    update_tools,
                }
            }
            Self::Secrets { subcommand } => match subcommand {
                SecretsCommands::Setup { provider, wasm_url } => {
                    Command::SecretsSetup { provider, wasm_url }
                }
            },
            Self::Runtime { subcommand } => match subcommand {
                RuntimeCommands::Oci { subcommand } => match subcommand {
                    OciCommands::Activate => Command::RuntimeOciActivate,
                },
            },
            Self::Tools { subcommand } => match subcommand {
                ToolsCommands::Download => Command::ToolsDownload,
                ToolsCommands::Activate => Command::ToolsActivate,
                ToolsCommands::List => Command::ToolsList,
            },
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

/// Parse command line arguments into a CLI structure.
#[must_use]
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
        assert!(!cli.llms); // Default llms is false
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Text);
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
        let cli_short = Cli::try_parse_from(["cuenv", "-L", "debug", "version"]).unwrap();
        assert!(matches!(cli_short.level, LogLevel::Debug));

        let cli_short = Cli::try_parse_from(["cuenv", "-L", "error", "version"]).unwrap();
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
        let cli = Cli::try_parse_from(["cuenv", "version", "--output", "json"]).unwrap();
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Json);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_cli_combined_flags() {
        let cli = Cli::try_parse_from([
            "cuenv", "--level", "debug", "--json", "version", "--output", "env",
        ])
        .unwrap();

        assert!(matches!(cli.level, LogLevel::Debug));
        assert!(cli.json);
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Env);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_command_conversion() {
        let version_cmd = Commands::Version {
            output_format: OutputFormat::Text,
        };
        let command: Command = version_cmd.into_command(None);
        match command {
            Command::Version { format } => assert_eq!(format, "text"),
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
        // With Optional command, missing subcommand parses successfully
        let cli = Cli::try_parse_from(["cuenv"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_llms_flag() {
        let cli = Cli::try_parse_from(["cuenv", "--llms"]).unwrap();
        assert!(cli.llms);
        assert!(cli.command.is_none());

        // --llms with a subcommand also works
        let cli = Cli::try_parse_from(["cuenv", "--llms", "version"]).unwrap();
        assert!(cli.llms);
        assert!(cli.command.is_some());
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

        if let Some(Commands::Env { subcommand }) = cli.command {
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
            "--output",
            "json",
        ])
        .unwrap();

        if let Some(Commands::Env { subcommand }) = cli.command {
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

        if let Some(Commands::Env { subcommand }) = cli.command {
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
        assert_eq!(OutputFormat::default(), OutputFormat::Text);

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
        let cli = Cli::try_parse_from(["cuenv", "version", "--output", "text"]).unwrap();
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Text);
        } else {
            panic!("Expected Version command");
        }

        let cli = Cli::try_parse_from(["cuenv", "version", "--output", "env"]).unwrap();
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Env);
        } else {
            panic!("Expected Version command");
        }

        let cli = Cli::try_parse_from(["cuenv", "version", "--output", "json"]).unwrap();
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Json);
        } else {
            panic!("Expected Version command");
        }

        // Test short form -o
        let cli = Cli::try_parse_from(["cuenv", "version", "-o", "rich"]).unwrap();
        if let Some(Commands::Version { output_format }) = cli.command {
            assert_eq!(output_format, OutputFormat::Rich);
        } else {
            panic!("Expected Version command");
        }
    }

    #[test]
    fn test_invalid_output_format() {
        let result = Cli::try_parse_from(["cuenv", "version", "--output", "invalid"]);
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

    #[test]
    fn test_output_format_display() {
        assert_eq!(OutputFormat::Json.to_string(), "json");
        assert_eq!(OutputFormat::Env.to_string(), "env");
        assert_eq!(OutputFormat::Text.to_string(), "text");
        assert_eq!(OutputFormat::Rich.to_string(), "rich");
    }

    #[test]
    fn test_output_format_as_ref() {
        assert_eq!(OutputFormat::Json.as_ref(), "json");
        assert_eq!(OutputFormat::Env.as_ref(), "env");
        assert_eq!(OutputFormat::Text.as_ref(), "text");
        assert_eq!(OutputFormat::Rich.as_ref(), "rich");
    }

    #[test]
    fn test_status_format_display() {
        assert_eq!(StatusFormat::Text.to_string(), "text");
        assert_eq!(StatusFormat::Short.to_string(), "short");
        assert_eq!(StatusFormat::Starship.to_string(), "starship");
    }

    #[test]
    fn test_status_format_default() {
        assert_eq!(StatusFormat::default(), StatusFormat::Text);
    }

    #[test]
    fn test_cli_error_with_help_method() {
        // Test adding help to Config error
        let config_err = CliError::config("original config error");
        let with_help = config_err.with_help("try running with --fix");
        if let CliError::Config { message, help } = with_help {
            assert_eq!(message, "original config error");
            assert_eq!(help, Some("try running with --fix".to_string()));
        } else {
            panic!("Expected Config error");
        }

        // Test adding help to Eval error
        let eval_err = CliError::eval("eval error");
        let with_help = eval_err.with_help("check your CUE syntax");
        if let CliError::Eval { message, help } = with_help {
            assert_eq!(message, "eval error");
            assert_eq!(help, Some("check your CUE syntax".to_string()));
        } else {
            panic!("Expected Eval error");
        }

        // Test adding help to Other error
        let other_err = CliError::other("other error");
        let with_help = other_err.with_help("contact support");
        if let CliError::Other { message, help } = with_help {
            assert_eq!(message, "other error");
            assert_eq!(help, Some("contact support".to_string()));
        } else {
            panic!("Expected Other error");
        }
    }

    #[test]
    fn test_cli_error_other_with_help() {
        let err = CliError::other_with_help("something went wrong", "try again later");
        if let CliError::Other { message, help } = err {
            assert_eq!(message, "something went wrong");
            assert_eq!(help, Some("try again later".to_string()));
        } else {
            panic!("Expected Other error");
        }
    }

    #[test]
    fn test_cuenv_core_io_error_with_path() {
        // Test I/O error with a path
        let io_err = cuenv_core::Error::Io {
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
            path: Some(std::path::Path::new("/etc/secrets").into()),
            operation: "write".to_string(),
        };
        let cli_err: CliError = io_err.into();
        let display = format!("{cli_err}");
        assert!(display.contains("I/O write failed"));
        assert!(display.contains("/etc/secrets"));
    }

    #[test]
    fn test_cuenv_core_tool_resolution_error_without_help() {
        let tool_err = cuenv_core::Error::ToolResolution {
            message: "tool not found".to_string(),
            help: None,
        };
        let cli_err: CliError = tool_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        let display = format!("{cli_err}");
        assert!(display.contains("tool not found"));
    }

    #[test]
    fn test_cuenv_core_tool_resolution_error_with_help() {
        let tool_err = cuenv_core::Error::ToolResolution {
            message: "tool not found".to_string(),
            help: Some("install via brew".to_string()),
        };
        let cli_err: CliError = tool_err.into();
        if let CliError::Eval { message, help } = cli_err {
            assert_eq!(message, "tool not found");
            assert_eq!(help, Some("install via brew".to_string()));
        } else {
            panic!("Expected Eval error");
        }
    }

    #[test]
    fn test_cuenv_core_platform_error() {
        let platform_err = cuenv_core::Error::Platform {
            message: "unsupported architecture".to_string(),
        };
        let cli_err: CliError = platform_err.into();
        assert!(matches!(cli_err, CliError::Eval { .. }));
        let display = format!("{cli_err}");
        assert!(display.contains("unsupported architecture"));
    }

    #[test]
    fn test_cuenv_core_utf8_error() {
        // Create an actual UTF-8 error by parsing invalid bytes
        let invalid_bytes = [0xff, 0xfe];
        let utf8_error = std::str::from_utf8(&invalid_bytes).unwrap_err();
        let utf8_err = cuenv_core::Error::Utf8 {
            source: utf8_error,
            file: None,
        };
        let cli_err: CliError = utf8_err.into();
        assert!(matches!(cli_err, CliError::Other { .. }));
    }

    #[test]
    fn test_commands_package_method() {
        // Test commands that have package parameter
        let task_cmd = Commands::Task {
            name: Some("build".to_string()),
            path: ".".to_string(),
            package: "mypackage".to_string(),
            labels: vec![],
            output_format: OutputFormat::Text,
            materialize_outputs: None,
            show_cache_path: false,
            backend: None,
            tui: false,
            interactive: false,
            help: false,
            all: false,
            skip_dependencies: false,
            task_args: vec![],
        };
        assert_eq!(task_cmd.package(), "mypackage");

        // Test commands without package parameter
        let version_cmd = Commands::Version {
            output_format: OutputFormat::Text,
        };
        assert_eq!(version_cmd.package(), "cuenv"); // default
    }

    #[test]
    fn test_task_command_with_labels() {
        let cli =
            Cli::try_parse_from(["cuenv", "task", "--label", "ci", "--label", "test", "build"])
                .unwrap();

        if let Some(Commands::Task { labels, name, .. }) = cli.command {
            assert_eq!(labels.len(), 2);
            assert!(labels.contains(&"ci".to_string()));
            assert!(labels.contains(&"test".to_string()));
            assert_eq!(name, Some("build".to_string()));
        } else {
            panic!("Expected Task command");
        }
    }

    #[test]
    fn test_task_command_interactive_flag() {
        let cli = Cli::try_parse_from(["cuenv", "task", "-i"]).unwrap();

        if let Some(Commands::Task { interactive, .. }) = cli.command {
            assert!(interactive);
        } else {
            panic!("Expected Task command");
        }
    }

    #[test]
    fn test_sync_lock_update_flag() {
        // Test -u alone (update all)
        let cli = Cli::try_parse_from(["cuenv", "sync", "lock", "-u"]).unwrap();
        if let Some(Commands::Sync {
            subcommand: Some(SyncCommands::Lock { update, .. }),
            ..
        }) = cli.command
        {
            // -u alone should give Some(vec![]) or Some(vec![""]) depending on clap behavior
            assert!(update.is_some());
        } else {
            panic!("Expected Sync Lock command");
        }
    }

    #[test]
    fn test_release_binaries_command() {
        let cli = Cli::try_parse_from([
            "cuenv",
            "release",
            "binaries",
            "--dry-run",
            "--build-only",
            "--target",
            "x86_64-unknown-linux-gnu,aarch64-apple-darwin",
        ])
        .unwrap();

        if let Some(Commands::Release {
            subcommand:
                ReleaseCommands::Binaries {
                    dry_run,
                    build_only,
                    target,
                    ..
                },
        }) = cli.command
        {
            assert!(dry_run);
            assert!(build_only);
            assert!(target.is_some());
            let targets = target.unwrap();
            assert_eq!(targets.len(), 2);
        } else {
            panic!("Expected Release Binaries command");
        }
    }

    #[test]
    fn test_changeset_add_package_parsing() {
        let cmd = Commands::Changeset {
            subcommand: ChangesetCommands::Add {
                path: ".".to_string(),
                summary: Some("test summary".to_string()),
                description: None,
                packages: vec![
                    "pkg-a:minor".to_string(),
                    "pkg-b:patch".to_string(),
                    "invalid".to_string(), // no colon, should be filtered
                ],
            },
        };

        let command = cmd.into_command(None);
        if let Command::ChangesetAdd { packages, .. } = command {
            assert_eq!(packages.len(), 2); // invalid one filtered out
            assert!(packages.contains(&("pkg-a".to_string(), "minor".to_string())));
            assert!(packages.contains(&("pkg-b".to_string(), "patch".to_string())));
        } else {
            panic!("Expected ChangesetAdd command");
        }
    }
}
