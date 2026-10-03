use super::OutputFormat;
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

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
    /// Regenerate infrastructure provider CUE types from cuenv.lock.
    #[command(about = "Regenerate infrastructure provider CUE types from cuenv.lock")]
    Infrastructure {
        /// Directory inside the CUE module whose cuenv.lock to read.
        #[arg(
            long,
            short = 'p',
            help = "Directory inside the CUE module whose cuenv.lock to read",
            default_value = "."
        )]
        path: String,
        /// Show what would change without writing files.
        #[arg(long, help = "Show what would change without writing files")]
        dry_run: bool,
        /// Fail when generated types are missing or differ.
        #[arg(long, help = "Fail when generated types are missing or differ")]
        check: bool,
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

/// Infrastructure subcommands (Terraform provider plugins + Turso state).
#[derive(Subcommand, Debug, Clone)]
pub enum InfrastructureCommands {
    /// Show the changes `apply` would make.
    #[command(about = "Show the changes `apply` would make")]
    Plan {
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
    /// Create, update, replace and delete resources to match the configuration.
    #[command(about = "Converge managed resources on the configuration")]
    Apply {
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
        /// Apply without an interactive confirmation.
        #[arg(
            long,
            short = 'y',
            alias = "auto-approve",
            help = "Apply without an interactive confirmation"
        )]
        yes: bool,
        /// Create resources of the selected environment even though the same
        /// addresses are recorded for the project without `--env`.
        #[arg(
            long,
            help = "With --env: create resources whose addresses are also recorded without --env, as separate objects"
        )]
        allow_separate_state: bool,
    },
    /// Delete every managed resource the project owns.
    #[command(about = "Delete every managed resource the project owns")]
    Destroy {
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
        /// Destroy without an interactive confirmation.
        #[arg(
            long,
            short = 'y',
            alias = "auto-approve",
            help = "Destroy without an interactive confirmation"
        )]
        yes: bool,
    },
    /// Inspect and repair recorded state (`list` when no subcommand is given).
    #[command(about = "Inspect and repair recorded state (`list` when no subcommand is given)")]
    State {
        /// State subcommand; `list` when omitted.
        #[command(subcommand)]
        subcommand: Option<InfrastructureStateCommands>,
        /// Path to directory containing CUE files.
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = ".",
            global = true
        )]
        path: String,
        /// Name of the CUE package to evaluate.
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv",
            global = true
        )]
        package: String,
    },
    /// Generate typed CUE for providers and pin them in cuenv.lock.
    #[command(about = "Generate typed CUE for providers and pin them in cuenv.lock")]
    Provider {
        /// Provider subcommand.
        #[command(subcommand)]
        subcommand: InfrastructureProviderCommands,
        /// Directory inside the CUE module that receives the types.
        #[arg(
            long,
            short = 'p',
            help = "Directory inside the CUE module that receives the types",
            default_value = ".",
            global = true
        )]
        path: String,
    },
    /// Show the project's state lock, or release it by identifier.
    #[command(about = "Show the project's state lock, or release it by identifier")]
    Unlock {
        /// Identifier of the lock to release; omit it to see who holds the lock.
        #[arg(help = "Identifier of the lock to release; omit it to see who holds the lock")]
        lock_identifier: Option<String>,
        /// Act on the lock of the project with this CUE module path instead of
        /// the evaluated project's (see `state locks`).
        #[arg(
            long,
            help = "CUE module path of the project whose lock to show or release, instead of the evaluated project's"
        )]
        module: Option<String>,
        /// Act on the lock of the project with this name instead of the
        /// evaluated project's (see `state locks`).
        #[arg(
            long,
            help = "Name of the project whose lock to show or release, instead of the evaluated project's"
        )]
        project: Option<String>,
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

/// `cuenv infrastructure provider` subcommands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum InfrastructureProviderCommands {
    /// Generate CUE types for a provider release and pin it in cuenv.lock.
    #[command(
        about = "Generate CUE types for a provider release and pin it in cuenv.lock",
        long_about = "Install a provider release, generate CUE types from its schema into the CUE \
                      module's cue.mod/gen, and pin its version, schema digest and archive \
                      checksums in cuenv.lock. Prints the imports and the `providers` entry to \
                      use. Your CUE files are not changed."
    )]
    Add {
        /// Provider source and exact version, such as `hashicorp/random@3.9.1`.
        #[arg(help = "Provider source and exact version, such as hashicorp/random@3.9.1")]
        release: String,
    },
    /// Delete a provider's generated CUE types and its cuenv.lock pin.
    #[command(about = "Delete a provider's generated CUE types and its cuenv.lock pin")]
    Remove {
        /// Provider source address, such as `hashicorp/random`.
        #[arg(help = "Provider source address, such as hashicorp/random")]
        source: String,
    },
}

/// `cuenv infrastructure state` subcommands.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum InfrastructureStateCommands {
    /// List managed resources recorded in state (the default).
    #[command(about = "List managed resources recorded in state (the default)")]
    List,
    /// List every lock held in the state database, for every project.
    #[command(about = "List every lock held in the state database, for every project")]
    Locks,
    /// Forget one managed resource without touching the real object.
    #[command(about = "Forget one managed resource without touching the real object")]
    Remove {
        /// Address of the resource, such as `random_pet.pet`.
        #[arg(help = "Address of the resource, such as `random_pet.pet`")]
        address: String,
    },
    /// Record changes an earlier run could not record and saved locally.
    #[command(about = "Record changes an earlier run could not record and saved locally")]
    Recover {
        /// Write a saved record even when the stored record changed since it
        /// was saved, overwriting the newer record.
        #[arg(
            long,
            help = "Write a saved record even when the stored record changed since it was saved, overwriting the newer record"
        )]
        force: bool,
        /// Accept a saved record that was saved for a different state backend
        /// (or without a backend binding) than the one in use.
        #[arg(
            long,
            help = "Accept a saved record that was saved for a different state backend than the one in use"
        )]
        accept_backend: bool,
    },
    /// Make this project's CUE instance the owner of its state.
    #[command(
        about = "Make this project's CUE instance (directory and package) the owner of its state"
    )]
    Adopt,
}

impl InfrastructureCommands {
    /// CUE package the subcommand evaluates.
    #[must_use]
    pub fn package(&self) -> &str {
        match self {
            Self::Plan { package, .. }
            | Self::Apply { package, .. }
            | Self::Destroy { package, .. }
            | Self::State { package, .. }
            | Self::Unlock { package, .. } => package,
            // Provider types are generated without evaluating CUE.
            Self::Provider { .. } => "cuenv",
        }
    }
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
    #[command(about = "Calculate and apply version bumps from changesets")]
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
