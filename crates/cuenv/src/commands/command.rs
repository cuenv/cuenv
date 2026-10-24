//! The [`Command`] enum: every CLI command with its parameters.

use clap_complete::Shell;
use cuenv_core::DryRun;

use super::{ci, infrastructure, sync};
use crate::cli::StatusFormat;

/// Represents all available CLI commands with their associated parameters.
///
/// Each variant corresponds to a subcommand in the CLI and contains
/// all the configuration needed to execute that command.
#[derive(Debug, Clone)]
pub enum Command {
    /// Display version information in the specified format.
    Version {
        /// Output format (e.g., "text", "json").
        format: String,
    },
    /// Display project information and metadata.
    Info {
        /// None = recursive (./...), Some(path) = specific directory only
        path: Option<String>,
        /// CUE package name to evaluate.
        package: String,
        /// Whether to show extended metadata.
        meta: bool,
    },
    /// Print resolved environment variables.
    EnvPrint {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (e.g., "text", "json", "export").
        format: String,
        /// Optional environment name to select.
        environment: Option<String>,
    },
    /// Load environment variables into the current shell.
    EnvLoad {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Check the status of environment variables and secrets.
    EnvStatus {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Whether to wait for all secrets to become available.
        wait: bool,
        /// Timeout in seconds when waiting.
        timeout: u64,
        /// Output format for status display.
        format: StatusFormat,
    },
    /// Inspect environment configuration details.
    EnvInspect {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Check environment configuration for errors.
    EnvCheck {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Target shell type for compatibility checking.
        shell: crate::cli::ShellType,
    },
    /// List available environments in the project.
    EnvList {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (e.g., "text", "json").
        format: String,
    },
    /// Execute one or more tasks with dependency resolution.
    Task {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific task name to run (None for interactive picker).
        name: Option<String>,
        /// Label filters to select tasks by labels.
        labels: Vec<String>,
        /// Optional environment name for task execution.
        environment: Option<String>,
        /// Output format (e.g., "text", "json").
        format: String,
        /// Path to materialize task outputs to.
        materialize_outputs: Option<String>,
        /// Whether to show the cache path for tasks.
        show_cache_path: bool,
        /// Execution backend override (e.g., "dagger").
        backend: Option<String>,
        /// Whether to use the TUI for task execution.
        tui: bool,
        /// Whether to run in interactive mode.
        interactive: bool,
        /// Whether to show help for the task.
        help: bool,
        /// Whether to skip running task dependencies.
        skip_dependencies: bool,
        /// Continue running independent siblings after a task fails.
        continue_on_error: bool,
        /// Dry run mode: export DAG without executing.
        dry_run: DryRun,
        /// Additional arguments to pass to the task.
        task_args: Vec<String>,
    },
    /// Execute an arbitrary command within the cuenv environment.
    Exec {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// The command to execute.
        command: String,
        /// Arguments to pass to the command.
        args: Vec<String>,
        /// Optional environment name to use.
        environment: Option<String>,
    },
    /// Format code based on formatters configuration.
    Fmt {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Apply formatting changes (default is check mode).
        fix: bool,
        /// Only run specific formatters.
        only: Option<Vec<String>>,
    },
    /// Initialize shell integration for the specified shell.
    ShellInit {
        /// Target shell type for initialization.
        shell: crate::cli::ShellType,
    },
    /// Allow a project's hooks to run.
    Allow {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Optional note explaining why the project is allowed.
        note: Option<String>,
        /// Skip confirmation prompt.
        yes: bool,
    },
    /// Deny a project's hooks from running.
    Deny {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Export environment variables in shell format.
    Export {
        /// Target shell format (e.g., "bash", "zsh", "fish").
        shell: Option<String>,
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
    },
    /// Run CI pipeline generation or execution.
    Ci {
        /// CI command arguments.
        args: ci::CiArgs,
    },
    /// Reserved for the future web-based user interface.
    Web {
        /// Port number for the future web server.
        port: u16,
        /// Host address for the future web server.
        host: String,
    },
    /// Add a new changeset entry.
    ChangesetAdd {
        /// Path to the repository root.
        path: String,
        /// Summary of the changes.
        summary: Option<String>,
        /// Detailed description of the changes.
        description: Option<String>,
        /// List of (package, bump type) pairs.
        packages: Vec<(String, String)>,
    },
    /// Show the status of pending changesets.
    ChangesetStatus {
        /// Path to the repository root.
        path: String,
        /// Output in JSON format.
        json: bool,
    },
    /// Generate changesets from commit history.
    ChangesetFromCommits {
        /// Path to the repository root.
        path: String,
        /// Git ref to start from.
        since: Option<String>,
    },
    /// Prepare a release by processing changesets.
    ReleasePrepare {
        /// Path to the repository root.
        path: String,
        /// Git ref to start from.
        since: Option<String>,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
        /// Branch name for the release PR.
        branch: String,
        /// Skip creating a pull request.
        no_pr: bool,
    },
    /// Update version numbers based on changesets.
    ReleaseVersion {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
    },
    /// Publish packages to their registries.
    ReleasePublish {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
    },
    /// Build and publish release binaries.
    ReleaseBinaries {
        /// Path to the repository root.
        path: String,
        /// Perform a dry run without making changes.
        dry_run: DryRun,
        /// Specific backends to use for building.
        backends: Option<Vec<String>>,
        /// Only build binaries, skip packaging and publishing.
        build_only: bool,
        /// Only package binaries, skip building and publishing.
        package_only: bool,
        /// Only publish binaries, skip building and packaging.
        publish_only: bool,
        /// Specific target platforms to build for.
        targets: Option<Vec<String>>,
        /// Version string override.
        version: Option<String>,
    },
    /// Generate shell completion scripts.
    Completions {
        /// Target shell for completions.
        shell: Shell,
    },
    /// Synchronize generated files with their sources.
    Sync {
        /// Provider name (None = sync all providers)
        subcommand: Option<String>,
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Operation mode (write, dry-run, check).
        mode: sync::SyncMode,
        /// Scope (single path or entire workspace).
        scope: sync::SyncScope,
        /// Show diff for codegen (codegen-specific).
        show_diff: bool,
        /// CI provider filter.
        ci_provider: Option<String>,
        /// Tools to force re-resolution for (lock-specific).
        /// - `None`: use cached resolutions from lockfile
        /// - `Some(vec![])`: re-resolve ALL tools (`-u` with no args)
        /// - `Some(vec!["bun"])`: re-resolve only specified tools
        update_tools: Option<Vec<String>>,
    },
    /// Set up a secrets provider for runtime secret resolution.
    SecretsSetup {
        /// The secrets provider to configure.
        provider: crate::cli::SecretsProvider,
        /// Custom WASM plugin URL for the provider.
        wasm_url: Option<String>,
    },
    /// Activate OCI runtime binaries.
    RuntimeOciActivate,
    /// Download tools for current platform.
    ToolsDownload,
    /// Activate tools (output shell exports).
    ToolsActivate,
    /// List configured tools.
    ToolsList,
    /// Build container images defined in CUE configuration.
    Build {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Image names to build (empty = list all).
        names: Vec<String>,
        /// Label filters to select images by labels.
        labels: Vec<String>,
    },
    /// Bring up long-running services defined in CUE configuration.
    Up {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific service names to bring up (empty = all).
        services: Vec<String>,
        /// Label filters to select services by labels.
        labels: Vec<String>,
        /// Optional environment name to apply to service env vars.
        environment: Option<String>,
    },
    /// Request shutdown of the active service session.
    Down {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Service names to stop; empty stops the whole active session.
        services: Vec<String>,
    },
    /// View service logs.
    Logs {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Specific service names to view logs for (empty = all).
        services: Vec<String>,
        /// Stream appended persisted log lines until the active session exits.
        follow: bool,
        /// Number of lines to show before following.
        lines: usize,
    },
    /// List running services and their status.
    Ps {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Output format (table or json).
        output_format: String,
    },
    /// Restart one or more services.
    Restart {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// Service names to restart.
        services: Vec<String>,
    },
    /// Plan, apply or inspect infrastructure managed through provider plugins.
    Infrastructure {
        /// Path to the CUE module or project directory.
        path: String,
        /// CUE package name to evaluate.
        package: String,
        /// What to do.
        action: infrastructure::InfrastructureAction,
        /// Complete named infrastructure configuration to select.
        environment: Option<String>,
    },
    /// Generate provider CUE types and pin providers in cuenv.lock.
    InfrastructureProvider {
        /// Directory inside the CUE module that receives the types.
        path: String,
        /// What to do.
        action: infrastructure::ProviderAction,
    },
}
