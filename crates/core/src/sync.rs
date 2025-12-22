//! Sync provider abstraction for file synchronization operations.
//!
//! This module defines the [`SyncProvider`] trait that allows different types of
//! file synchronization (ignore files, codeowners, cubes, CI workflows) to be
//! implemented in their respective crates and used uniformly by the CLI.
//!
//! # Architecture
//!
//! Each sync provider crate (cuenv-ignore, cuenv-codeowners, etc.) implements
//! the [`SyncProvider`] trait. The CLI loads the CUE module once and passes it
//! to providers, avoiding redundant evaluation.
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_core::sync::{SyncProvider, SyncOptions, SyncContext};
//!
//! // CLI loads module once
//! let module = load_module(&cwd, package)?;
//!
//! // Pass to each provider
//! for provider in providers {
//!     let result = provider.sync(&SyncContext {
//!         module: &module,
//!         options: &options,
//!     }).await?;
//! }
//! ```

use crate::Result;
use crate::manifest::Base;
use crate::module::ModuleEvaluation;
use async_trait::async_trait;
use std::path::Path;

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
///
/// These options control how synchronization behaves across all providers.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// The sync operation mode.
    pub mode: SyncMode,
    /// Show diff for files that would change.
    pub show_diff: bool,
    /// Overwrite existing files without prompting (only applies in Write mode).
    pub force: bool,
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

    /// Create an empty result (no output, no error).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            output: String::new(),
            had_error: false,
        }
    }
}

/// Context for sync operations.
///
/// Provides access to the evaluated CUE module and sync options.
pub struct SyncContext<'a> {
    /// The evaluated CUE module containing all instances.
    pub module: &'a ModuleEvaluation,
    /// Options controlling sync behavior.
    pub options: &'a SyncOptions,
    /// Package name being synced.
    pub package: &'a str,
}

/// Trait for sync providers.
///
/// Each provider (ignore, cubes, codeowners, ci) implements this trait
/// to handle synchronization of its specific file type.
///
/// # Implementors
///
/// - `cuenv-ignore`: Generates .gitignore, .dockerignore, etc.
/// - `cuenv-codeowners`: Generates CODEOWNERS files
/// - `cuenv-cubes`: Generates files from CUE cube templates
/// - `cuenv-ci`: Generates CI workflow files
#[async_trait]
pub trait SyncProvider: Send + Sync {
    /// Name of the sync provider (e.g., "ignore", "cubes").
    ///
    /// Used as the CLI subcommand name.
    fn name(&self) -> &'static str;

    /// Description for CLI help.
    fn description(&self) -> &'static str;

    /// Check if this provider has configuration for the given manifest.
    ///
    /// Used to determine which providers to run when syncing all.
    fn has_config(&self, manifest: &Base) -> bool;

    /// Sync files for a single path within the module.
    ///
    /// The path should be relative to the module root.
    async fn sync_path(&self, path: &Path, ctx: &SyncContext<'_>) -> Result<SyncResult>;

    /// Sync all applicable paths in the module.
    ///
    /// Iterates through all instances in the module that have configuration
    /// for this provider and syncs each one.
    async fn sync_all(&self, ctx: &SyncContext<'_>) -> Result<SyncResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_result_success() {
        let result = SyncResult::success("Created .gitignore");
        assert_eq!(result.output, "Created .gitignore");
        assert!(!result.had_error);
    }

    #[test]
    fn test_sync_result_error() {
        let result = SyncResult::error("Failed to write file");
        assert_eq!(result.output, "Failed to write file");
        assert!(result.had_error);
    }

    #[test]
    fn test_sync_result_empty() {
        let result = SyncResult::empty();
        assert!(result.output.is_empty());
        assert!(!result.had_error);
    }

    #[test]
    fn test_sync_options_default() {
        let options = SyncOptions::default();
        assert_eq!(options.mode, SyncMode::Write);
        assert!(!options.show_diff);
        assert!(!options.force);
    }

    #[test]
    fn test_sync_mode_default() {
        assert_eq!(SyncMode::default(), SyncMode::Write);
    }
}
