mod command_conversion;
mod error;
mod output;

use crate::completions::task_completer;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use serde::{Deserialize, Serialize};

pub use error::{CliError, EXIT_CLI, EXIT_EVAL, EXIT_OK, exit_code_for, render_error};
pub use output::{ErrorEnvelope, OkEnvelope, OutputFormat};

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
        /// If not specified, uses config.commands.task.list.format or auto-detects based on TTY.
        #[arg(
            long = "output",
            short = 'o',
            help = "Output format for task listing (defaults to config or auto-detect)",
            value_enum
        )]
        output_format: Option<OutputFormat>,
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
        /// Skip executing task dependencies (for CI orchestrators that handle deps externally).
        #[arg(
            long = "skip-dependencies",
            short = 'S',
            help = "Skip executing task dependencies (for CI orchestrators that handle deps externally)",
            default_value_t = false
        )]
        skip_dependencies: bool,
        /// Continue running independent siblings after a task fails; dependents become Skipped.
        #[arg(
            long = "continue-on-error",
            help = "Don't abort on first failure; dependents of the failing task are emitted as task.skipped and unrelated siblings keep running",
            default_value_t = false
        )]
        continue_on_error: bool,
        /// Dry run mode: export task DAG as JSON without executing.
        #[arg(
            long = "dry-run",
            short = 'n',
            help = "Export task dependency graph as JSON without executing tasks",
            default_value_t = false
        )]
        dry_run: bool,
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
    Ci(crate::commands::ci::CiArgs),
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
    /// Build container images defined in CUE configuration.
    #[command(about = "Build container images defined in CUE configuration")]
    Build {
        /// Image names to build (default: list all).
        #[arg(help = "Image names to build (default: list all)")]
        names: Vec<String>,
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
        /// Filter images by label (repeatable).
        #[arg(
            long = "label",
            short = 'l',
            action = clap::ArgAction::Append,
            help = "Filter images by label (repeatable)",
            value_name = "LABEL"
        )]
        labels: Vec<String>,
    },
    /// Bring up long-running services.
    #[command(about = "Bring up long-running services defined in CUE configuration")]
    Up {
        /// Service names to bring up (default: all).
        #[arg(help = "Service names to bring up (default: all)")]
        services: Vec<String>,
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
        /// Filter services by label (repeatable).
        #[arg(
            long = "label",
            short = 'l',
            action = clap::ArgAction::Append,
            help = "Filter services by label (repeatable)",
            value_name = "LABEL"
        )]
        labels: Vec<String>,
    },
    /// Tear down running services.
    #[command(about = "Tear down running services")]
    Down {
        /// Service names to bring down (default: all).
        #[arg(help = "Service names to bring down (default: all)")]
        services: Vec<String>,
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
    /// View service logs.
    #[command(about = "View service logs")]
    Logs {
        /// Service names to view logs for (default: all).
        #[arg(help = "Service names to view logs for (default: all)")]
        services: Vec<String>,
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
        /// Follow log output.
        #[arg(long, short = 'f', help = "Follow log output")]
        follow: bool,
        /// Number of lines to show.
        #[arg(
            long,
            short = 'n',
            help = "Number of lines to show",
            default_value = "100"
        )]
        lines: usize,
    },
    /// List running services.
    #[command(about = "List running services and their status")]
    Ps {
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
        /// Output format (table or json).
        #[arg(
            long,
            help = "Output format",
            default_value = "table",
            rename_all = "kebab-case"
        )]
        output_format: String,
    },
    /// Restart a service.
    #[command(about = "Restart one or more services")]
    Restart {
        /// Service names to restart.
        #[arg(required = true, help = "Service names to restart")]
        services: Vec<String>,
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
    /// Sync files from CUE codegen configurations in projects.
    #[command(about = "Sync files from CUE codegen configurations in projects")]
    Codegen {
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
        /// Sync codegen for all projects in the workspace.
        #[arg(
            long = "all",
            short = 'A',
            help = "Sync codegen for all projects in the workspace"
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
    /// Sync VCS dependencies from CUE configuration.
    #[command(about = "Sync VCS dependencies from CUE configuration")]
    Vcs {
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
        /// Show what would be synced without writing files.
        #[arg(long, help = "Show what would be synced without writing files")]
        dry_run: bool,
        /// Check if VCS dependencies are in sync without making changes.
        #[arg(
            long,
            help = "Check if VCS dependencies are in sync without making changes"
        )]
        check: bool,
        /// Sync VCS dependencies for all projects in the workspace.
        #[arg(
            long = "all",
            short = 'A',
            help = "Sync VCS dependencies for all projects in the workspace"
        )]
        all: bool,
        /// Force re-resolution of VCS refs. Use without arguments to update all, or specify names.
        #[arg(
            long = "update",
            short = 'u',
            help = "Force re-resolution of VCS refs. Use -u to update all, or -u mylib to update specific dependencies.",
            num_args = 0..,
            value_name = "NAMES",
            default_missing_value = ""
        )]
        update: Option<Vec<String>>,
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
        /// Override the default WASM URL (for providers that use a WASM setup step).
        #[arg(
            long,
            help = "Override the default WASM URL (for providers that use a WASM setup step)",
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
    /// Infisical (checks REST API authentication environment).
    Infisical,
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
    /// Activate tools (output inferred shell exports from lockfile tool metadata).
    #[command(about = "Activate tools (output inferred shell exports from lockfile tool metadata)")]
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
            | Self::Allow { package, .. }
            | Self::Deny { package, .. }
            | Self::Export { package, .. }
            | Self::Build { package, .. }
            | Self::Up { package, .. }
            | Self::Down { package, .. }
            | Self::Logs { package, .. }
            | Self::Ps { package, .. }
            | Self::Restart { package, .. } => package,

            // Nested env subcommands with --package
            Self::Env { subcommand } => match subcommand {
                EnvCommands::Print { package, .. }
                | EnvCommands::Load { package, .. }
                | EnvCommands::Status { package, .. }
                | EnvCommands::Inspect { package, .. }
                | EnvCommands::Check { package, .. }
                | EnvCommands::List { package, .. } => package,
            },

            Self::Sync {
                subcommand,
                package,
                ..
            } => match subcommand {
                Some(
                    SyncCommands::Lock { package, .. }
                    | SyncCommands::Codegen { package, .. }
                    | SyncCommands::Ci { package, .. }
                    | SyncCommands::Vcs { package, .. },
                ) if package != "cuenv" => package,
                Some(_) | None => package,
            },

            // Commands that don't use CUE evaluation or have no package param
            Self::Version { .. }
            | Self::Completions { .. }
            | Self::Shell { .. }
            | Self::Ci { .. }
            | Self::Web { .. }
            | Self::Changeset { .. }
            | Self::Release { .. }
            | Self::Secrets { .. }
            | Self::Runtime { .. }
            | Self::Tools { .. } => "cuenv",
        }
    }
}

/// Generate shell completions using `clap_complete`'s dynamic completion system
///
/// The binary itself handles completion requests via environment variables.
/// This function outputs instructions for the user to set up completions.
#[allow(clippy::print_stdout)] // Static completion instructions, no secrets
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
#[path = "cli_tests.rs"]
mod tests;
