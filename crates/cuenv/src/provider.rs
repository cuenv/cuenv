//! Provider traits for extensible cuenv functionality.
//!
//! This module defines the core provider system that allows cuenv to be extended
//! with custom capabilities. Providers implement one or more capability traits
//! and are registered via the [`CuenvBuilder`](crate::CuenvBuilder).
//!
//! # Architecture
//!
//! - [`Provider`] - Base trait that all providers must implement
//! - [`SyncCapability`] - For providers that sync files from CUE configuration
//! - [`RuntimeCapability`] - For providers that execute tasks (future)
//! - [`SecretCapability`] - For providers that resolve secrets (future)
//!
//! # Example: Single-Capability Provider
//!
//! ```ignore
//! use cuenv::{Provider, SyncCapability};
//!
//! pub struct CiProvider;
//!
//! impl Provider for CiProvider {
//!     fn name(&self) -> &'static str { "ci" }
//!     fn description(&self) -> &'static str { "CI workflow sync" }
//! }
//!
//! // Also implement SyncCapability...
//! ```
//!
//! # Example: Multi-Capability Provider
//!
//! ```ignore
//! use cuenv::{Provider, SyncCapability, RuntimeCapability};
//!
//! pub struct DaggerProvider;
//!
//! impl Provider for DaggerProvider {
//!     fn name(&self) -> &'static str { "dagger" }
//!     fn description(&self) -> &'static str { "Dagger sync and execution" }
//! }
//!
//! // Implement both SyncCapability and RuntimeCapability...
//! ```

use async_trait::async_trait;
use clap::Command;
use cuenv_core::Result;
use cuenv_core::manifest::Base;
use std::any::Any;
use std::path::Path;

use crate::commands::CommandExecutor;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncResult};

/// Base trait for all providers.
///
/// Every provider must implement this trait. Providers then implement one or more
/// capability traits ([`SyncCapability`], [`RuntimeCapability`], [`SecretCapability`])
/// to define their functionality.
///
/// # Thread Safety
///
/// Providers must be `Send + Sync` to allow concurrent execution.
pub trait Provider: Send + Sync + 'static {
    /// Unique name identifying this provider.
    ///
    /// Used as the CLI subcommand name (e.g., "ci" for `cuenv sync ci`).
    fn name(&self) -> &'static str;

    /// Human-readable description for CLI help.
    fn description(&self) -> &'static str;

    /// Returns self as `Any` for capability detection.
    ///
    /// This enables the registry to detect which capabilities a provider implements
    /// at runtime. The default implementation should work for most providers.
    fn as_any(&self) -> &dyn Any;

    /// Returns self as mutable `Any` for capability detection.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Capability for syncing files from CUE configuration.
///
/// Providers implementing this trait can:
/// - Sync files for a single path (`sync_path`)
/// - Sync files across the entire workspace (`sync_workspace`)
/// - Provide custom CLI arguments (`build_sync_command`)
///
/// # Built-in Providers
///
/// - `CiProvider` - Syncs CI workflow files (GitHub Actions, Buildkite)
/// - `CubesProvider` - Syncs cube-generated project files
/// - `RulesProvider` - Syncs rules configuration (.gitignore, .editorconfig, CODEOWNERS)
#[async_trait]
pub trait SyncCapability: Provider {
    /// Build CLI subcommand for this sync provider.
    ///
    /// Override to add provider-specific arguments.
    fn build_sync_command(&self) -> Command;

    /// Sync a single path.
    ///
    /// Called when running `cuenv sync <provider> -p <path>`.
    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult>;

    /// Sync all projects in the workspace.
    ///
    /// Called when running `cuenv sync <provider> -A`.
    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult>;

    /// Check if this provider has config at the given manifest.
    ///
    /// Used to determine which providers to run when syncing all.
    fn has_config(&self, manifest: &Base) -> bool;

    /// Parse provider-specific args from CLI matches.
    ///
    /// Override to handle custom arguments.
    fn parse_sync_args(&self, matches: &clap::ArgMatches) -> SyncOptions {
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
        }
    }
}

/// Capability for executing tasks.
///
/// Providers implementing this trait can execute tasks using custom backends
/// (e.g., Dagger containers, Nix shells, remote execution).
///
/// # Future Development
///
/// This trait is a placeholder for future task execution backends.
#[async_trait]
pub trait RuntimeCapability: Provider {
    /// Execute a task and return the output.
    async fn execute_task(
        &self,
        task_name: &str,
        executor: &CommandExecutor,
    ) -> Result<String>;

    /// Check if this runtime can handle the given task.
    fn can_handle(&self, task_name: &str) -> bool;
}

/// Capability for resolving secrets.
///
/// Providers implementing this trait can resolve secrets from various backends
/// (e.g., 1Password, Vault, AWS Secrets Manager).
///
/// # Future Development
///
/// This trait is a placeholder for future secret resolution backends.
#[async_trait]
pub trait SecretCapability: Provider {
    /// Resolve a secret reference and return the value.
    ///
    /// The reference format is provider-specific (e.g., `op://vault/item/field`
    /// for 1Password).
    async fn resolve(&self, reference: &str) -> Result<String>;

    /// Check if this provider can handle the given reference.
    fn can_resolve(&self, reference: &str) -> bool;
}

/// Macro to implement `as_any` methods for a provider.
///
/// This is a convenience macro to avoid boilerplate in provider implementations.
///
/// # Example
///
/// ```ignore
/// use cuenv::impl_provider_any;
///
/// pub struct MyProvider;
///
/// impl_provider_any!(MyProvider);
///
/// impl Provider for MyProvider {
///     fn name(&self) -> &'static str { "my-provider" }
///     fn description(&self) -> &'static str { "My custom provider" }
///     // as_any and as_any_mut are implemented by the macro
/// }
/// ```
#[macro_export]
macro_rules! impl_provider_any {
    ($t:ty) => {
        impl $t {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestProvider;

    impl Provider for TestProvider {
        fn name(&self) -> &'static str {
            "test"
        }

        fn description(&self) -> &'static str {
            "Test provider"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn test_provider_name() {
        let provider = TestProvider;
        assert_eq!(provider.name(), "test");
    }

    #[test]
    fn test_provider_description() {
        let provider = TestProvider;
        assert_eq!(provider.description(), "Test provider");
    }

    #[test]
    fn test_provider_as_any() {
        let provider = TestProvider;
        let any = provider.as_any();
        assert!(any.is::<TestProvider>());
    }
}
