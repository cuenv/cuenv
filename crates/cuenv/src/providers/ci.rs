//! CI sync provider.
//!
//! Syncs CI workflow files (GitHub Actions, Buildkite) from CUE configuration.

use std::any::Any;
use std::path::Path;

use async_trait::async_trait;
use clap::{Arg, Command, arg};
use cuenv_core::Result;
use cuenv_core::manifest::Base;

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncResult};
use crate::provider::{Provider, SyncCapability};

/// CI workflow sync provider.
///
/// Syncs CI workflow files from CUE configuration. Supports:
/// - GitHub Actions
/// - Buildkite
///
/// # Example
///
/// ```ignore
/// use cuenv::Cuenv;
/// use cuenv::providers::CiProvider;
///
/// Cuenv::builder()
///     .with_sync_provider(CiProvider::new())
///     .build()
///     .run()
/// ```
pub struct CiProvider;

impl CiProvider {
    /// Create a new CI provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for CiProvider {
    fn name(&self) -> &'static str {
        "ci"
    }

    fn description(&self) -> &'static str {
        "Sync CI workflow files (GitHub Actions, Buildkite)"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[async_trait]
impl SyncCapability for CiProvider {
    fn build_sync_command(&self) -> Command {
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
            .arg(
                Arg::new("provider")
                    .long("provider")
                    .help("Filter to specific provider (github, buildkite)")
                    .value_name("PROVIDER"),
            )
    }

    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let path_str = path.to_str().ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Path contains invalid UTF-8: {}",
                path.display()
            ))
        })?;

        let ci_options = functions::CiSyncOptions {
            dry_run: dry_run.into(),
            check,
            provider: options.ci_provider.as_deref(),
        };
        let request = functions::CiSyncRequest {
            path: path_str,
            package,
            options: ci_options,
        };
        let output = functions::execute_sync_ci(request, executor).await?;

        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let ci_options = functions::CiSyncOptions {
            dry_run: dry_run.into(),
            check,
            provider: options.ci_provider.as_deref(),
        };
        let request = functions::CiWorkspaceSyncRequest {
            package,
            options: ci_options,
        };
        let output = functions::execute_sync_ci_workspace(request, executor).await?;

        Ok(SyncResult::success(output))
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // CI config is on Project, not Base
        // For simplicity, we'll check during sync
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_provider_name() {
        let provider = CiProvider::new();
        assert_eq!(provider.name(), "ci");
    }

    #[test]
    fn test_ci_provider_description() {
        let provider = CiProvider::new();
        assert!(!provider.description().is_empty());
        assert!(provider.description().contains("CI"));
    }

    #[test]
    fn test_ci_provider_as_any() {
        let provider = CiProvider::new();
        let any = provider.as_any();
        assert!(any.is::<CiProvider>());
    }

    #[test]
    fn test_ci_provider_as_any_mut() {
        let mut provider = CiProvider::new();
        let any = provider.as_any_mut();
        assert!(any.is::<CiProvider>());
    }

    #[test]
    fn test_ci_provider_command() {
        let provider = CiProvider::new();
        let cmd = provider.build_sync_command();
        assert_eq!(cmd.get_name(), "ci");
    }

    #[test]
    fn test_ci_provider_command_has_args() {
        let provider = CiProvider::new();
        let cmd = provider.build_sync_command();

        let args: Vec<_> = cmd.get_arguments().map(|a| a.get_id().as_str()).collect();
        assert!(args.contains(&"path"));
        assert!(args.contains(&"package"));
        assert!(args.contains(&"dry-run"));
        assert!(args.contains(&"check"));
        assert!(args.contains(&"all"));
        assert!(args.contains(&"provider"));
    }

    #[test]
    fn test_ci_provider_default() {
        let provider = CiProvider;
        assert_eq!(provider.name(), "ci");
    }

    #[test]
    fn test_ci_provider_has_config() {
        let provider = CiProvider::new();
        let base = Base::default();
        // CI config is on Project, not Base, so this returns false
        assert!(!provider.has_config(&base));
    }
}
