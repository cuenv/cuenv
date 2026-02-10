//! Sync provider trait for extensible file synchronization.
//!
//! This module defines the `SyncProvider` trait that allows different types of
//! file synchronization (ignore files, codeowners, codegen, CI workflows) to be
//! registered and executed uniformly.

use async_trait::async_trait;
use clap::{Arg, Command, arg};
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::path::Path;

use super::super::CommandExecutor;

/// Mode of operation for sync commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SyncMode {
    /// Actually write files to disk.
    #[default]
    Write,
    /// Preview what would change without writing files.
    DryRun,
    /// Check if files are in sync (error if not).
    Check,
}

/// Options passed to sync operations.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// The sync operation mode.
    pub mode: SyncMode,
    /// Show diff for files that would change (codegen-specific).
    pub show_diff: bool,
    /// CI provider filter (github, buildkite).
    pub ci_provider: Option<String>,
    /// Tools to force re-resolution for (lock-specific).
    /// - `None`: use cached resolutions from lockfile
    /// - `Some(vec![])`: re-resolve ALL tools (`-u` with no args)
    /// - `Some(vec!["bun"])`: re-resolve only specified tools
    pub update_tools: Option<Vec<String>>,
}

/// Result of a sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Output message describing what was synced.
    pub output: String,
    /// Whether any errors occurred during sync.
    pub had_error: bool,
}

impl SyncResult {
    /// Create a successful sync result.
    #[must_use]
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            had_error: false,
        }
    }

    /// Create an error sync result.
    #[must_use]
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            had_error: true,
        }
    }
}

/// Trait for sync providers (codegen, ignore, codeowners, ci).
///
/// Each provider implements this trait to handle synchronization of a specific
/// type of generated file. Providers are registered with a `SyncRegistry` and
/// can be invoked individually or collectively via `cuenv sync`.
#[async_trait]
pub trait SyncProvider: Send + Sync {
    /// Name of the sync provider (e.g., "codegen", "ignore").
    ///
    /// This is used as the CLI subcommand name.
    fn name(&self) -> &'static str;

    /// Description for CLI help.
    fn description(&self) -> &'static str;

    /// Sync a single path.
    ///
    /// Called when running `cuenv sync <provider>` without `-A`.
    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult>;

    /// Sync all projects in workspace (when -A is used).
    ///
    /// Called when running `cuenv sync <provider> -A` or `cuenv sync -A`.
    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult>;

    /// Check if this provider has config at the given path.
    ///
    /// Used to determine which providers to run when syncing all.
    fn has_config(&self, manifest: &Base) -> bool;

    /// Build CLI subcommand for this provider.
    ///
    /// Override to add provider-specific arguments.
    fn build_command(&self) -> Command {
        self.default_command()
    }

    /// Build default CLI subcommand with common arguments.
    fn default_command(&self) -> Command {
        Command::new(self.name())
            .about(self.description())
            .arg(arg!(-p --path <PATH> "Path to directory containing CUE files").default_value("."))
            .arg(
                Arg::new("package")
                    .long("package")
                    .help("Name of the CUE package to evaluate")
                    .default_value("cuenv"),
            )
            .arg(arg!(--"dry-run" "Show what would be generated without writing files"))
            .arg(arg!(--check "Check if files are in sync without making changes"))
            .arg(arg!(-A --all "Sync all projects in the workspace"))
    }

    /// Parse provider-specific args from matches.
    ///
    /// Override to handle custom arguments.
    fn parse_args(&self, matches: &clap::ArgMatches) -> SyncOptions {
        let mode = if matches.get_flag("dry-run") {
            SyncMode::DryRun
        } else if matches.get_flag("check") {
            SyncMode::Check
        } else {
            SyncMode::Write
        };

        SyncOptions {
            mode,
            show_diff: matches.get_flag("diff"),
            ci_provider: matches.get_one::<String>("provider").cloned(),
            update_tools: None,
        }
    }
}
