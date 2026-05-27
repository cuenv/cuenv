use super::{
    ChangesetCommands, EnvCommands, OutputFormat, ReleaseCommands, RuntimeCommands,
    SecretsCommands, ShellCommands, SyncCommands, ToolsCommands,
};
use crate::completions::task_completer;
use clap::Subcommand;
use clap_complete::Shell;

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
    /// Request shutdown of the active service session.
    #[command(about = "Request shutdown of the active service session")]
    Down {
        /// Reserved for named-service shutdown; omit to stop all services.
        #[arg(help = "Reserved for named-service shutdown; omit to stop all services")]
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
        /// Stream appended persisted log lines until the active session exits.
        #[arg(
            long,
            short = 'f',
            help = "Stream appended persisted log lines until the active session exits"
        )]
        follow: bool,
        /// Number of lines to show before following.
        #[arg(
            long,
            short = 'n',
            help = "Number of lines to show before following",
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
