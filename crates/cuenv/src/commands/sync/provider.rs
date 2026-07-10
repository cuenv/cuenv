//! Internal contract for built-in sync operations.
//!
//! CLI parsing stays in `crate::cli`; providers only own sync behavior. Keeping
//! the registry internal ensures the public API does not imply that external
//! providers participate in the real command dispatcher.

use async_trait::async_trait;
use cuenv_core::Result;
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

/// Scope of a sync operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyncScope {
    /// Sync one project path.
    #[default]
    Path,
    /// Sync every project in the discovered workspace.
    Workspace,
}

/// Shared input passed to a built-in sync provider.
#[derive(Clone, Copy)]
pub struct SyncRequest<'a> {
    /// Path selected by the user.
    pub path: &'a Path,
    /// CUE package to evaluate.
    pub package: &'a str,
    /// Provider-specific options normalized by CLI parsing.
    pub options: &'a SyncOptions,
    /// Whether to sync one path or the whole workspace.
    pub scope: SyncScope,
    /// Shared command executor and module cache.
    pub executor: &'a CommandExecutor,
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
}

/// Built-in sync operation executed by the canonical registry.
///
/// This is deliberately crate-private. CLI subcommands are defined statically,
/// and every registered provider is shipped with the cuenv binary.
#[async_trait]
pub trait SyncProvider: Send + Sync {
    /// Name of the sync provider (e.g., "codegen", "ignore").
    fn name(&self) -> &'static str;

    /// Execute the provider for the requested scope.
    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult>;
}
