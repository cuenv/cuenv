// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
// https://github.com/rust-lang/rust/issues/147648
#![allow(unused_assignments)]

//! cuenv - CUE-powered environment management library
//!
//! This crate provides a library-first architecture for cuenv, allowing external
//! crates to extend functionality by registering custom providers.
//!
//! # Architecture
//!
//! cuenv uses a unified provider system where providers implement one or more
//! capability traits:
//!
//! - [`SyncCapability`] - Sync files from CUE configuration
//! - [`RuntimeCapability`] - Execute tasks (future)
//! - [`SecretCapability`] - Resolve secrets (future)
//!
//! # Example: Custom CLI with Additional Providers
//!
//! ```ignore
//! use cuenv::{Cuenv, SyncCapability};
//!
//! fn main() -> cuenv::Result<()> {
//!     Cuenv::builder()
//!         .with_defaults()
//!         .with_sync_provider(my_provider::CustomProvider::new())
//!         .build()
//!         .run()
//! }
//! ```
//!
//! # Example: Multi-Capability Provider
//!
//! A single provider can implement multiple capabilities:
//!
//! ```ignore
//! use cuenv::{Provider, SyncCapability, RuntimeCapability};
//!
//! pub struct DaggerProvider;
//!
//! impl Provider for DaggerProvider {
//!     fn name(&self) -> &'static str { "dagger" }
//!     fn description(&self) -> &'static str { "Dagger-based sync and execution" }
//!     fn as_any(&self) -> &dyn std::any::Any { self }
//!     fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
//! }
//!
//! // Implement both SyncCapability and RuntimeCapability...
//! ```

// CLI binary needs to output to stdout/stderr - this is intentional
// expect_used is allowed for infallible operations like writing to strings
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::expect_used)]

mod builder;
/// CLI argument parsing and exit codes.
pub mod cli;
/// Command implementations (task, env, sync, etc.).
pub mod commands;
/// Shell completion generation.
pub mod completions;
/// Multi-process event coordination.
pub mod coordinator;
/// Event handling and routing.
pub mod events;
/// Performance measurement utilities.
pub mod performance;
/// Provider trait definitions.
pub mod provider;
/// Built-in provider implementations.
pub mod providers;
/// Provider registration and lookup.
pub mod registry;
/// Tracing and logging configuration.
pub mod tracing;
/// Terminal UI components.
pub mod tui;

// Re-export public API
pub use builder::CuenvBuilder;
pub use cuenv_core::Result;
pub use provider::{Provider, RuntimeCapability, SecretCapability, SyncCapability};
pub use registry::ProviderRegistry;

use crate::cli::EXIT_OK;

/// The main cuenv application.
///
/// Use [`Cuenv::builder()`] to create a new instance with custom providers,
/// or [`Cuenv::with_defaults()`] for the standard configuration.
pub struct Cuenv {
    /// The provider registry containing all registered providers.
    pub registry: ProviderRegistry,
}

impl Cuenv {
    /// Create a new builder for configuring cuenv.
    #[must_use]
    pub fn builder() -> CuenvBuilder {
        CuenvBuilder::new()
    }

    /// Create cuenv with default providers (ci, cubes, rules).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::builder().with_defaults().build()
    }

    /// Build the `sync` subcommand dynamically from registered providers.
    ///
    /// Each sync provider contributes a subcommand via its `build_sync_command()` method.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let cuenv = Cuenv::with_defaults();
    /// let sync_cmd = cuenv.build_sync_command();
    /// // sync_cmd has subcommands: ci, cubes, rules
    /// ```
    #[must_use]
    pub fn build_sync_command(&self) -> clap::Command {
        use clap::{Arg, Command};

        let mut sync_cmd = Command::new("sync")
            .about("Sync generated files from CUE configuration")
            .arg(
                Arg::new("path")
                    .long("path")
                    .short('p')
                    .help("Path to directory containing CUE files")
                    .default_value("."),
            )
            .arg(
                Arg::new("package")
                    .long("package")
                    .help("Name of the CUE package to evaluate")
                    .default_value("cuenv"),
            )
            .arg(
                Arg::new("dry-run")
                    .long("dry-run")
                    .help("Show what would be generated without writing files")
                    .action(clap::ArgAction::SetTrue)
                    .global(true),
            )
            .arg(
                Arg::new("check")
                    .long("check")
                    .help("Check if files are in sync without making changes")
                    .action(clap::ArgAction::SetTrue)
                    .global(true),
            )
            .arg(
                Arg::new("all")
                    .long("all")
                    .short('A')
                    .help("Sync all projects in the workspace")
                    .action(clap::ArgAction::SetTrue)
                    .global(true),
            );

        // Add subcommands from registered sync providers
        for provider in self.registry.sync_providers() {
            sync_cmd = sync_cmd.subcommand(provider.build_sync_command());
        }

        sync_cmd
    }

    /// Run the cuenv CLI (placeholder).
    ///
    /// **Note**: This method is a placeholder for future library-driven CLI execution.
    /// Currently, the full CLI logic remains in the binary (`main.rs`).
    ///
    /// For now, use the `cuenv` binary directly or the individual provider APIs
    /// like [`build_sync_command()`](Self::build_sync_command).
    ///
    /// # Errors
    ///
    /// Returns an error if command execution fails.
    #[doc(hidden)]
    pub fn run(self) -> Result<()> {
        // Placeholder: In the future, this will parse args using build_sync_command()
        // and dispatch to providers. For now, return success as the binary handles CLI.
        let exit_code = run_cli_with_registry(self);
        if exit_code == EXIT_OK {
            Ok(())
        } else {
            Err(cuenv_core::Error::configuration(format!(
                "Command failed with exit code {exit_code}"
            )))
        }
    }
}

/// Run the CLI with the given cuenv instance.
///
/// This is a placeholder that will be expanded to use the registry
/// for dynamic CLI generation in the future.
fn run_cli_with_registry(_cuenv: Cuenv) -> i32 {
    // For now, this just returns OK
    // The actual CLI logic remains in main.rs
    // Future: Use cuenv.registry to build dynamic CLI
    EXIT_OK
}

/// Exit code for SIGINT (128 + signal number 2)
pub const EXIT_SIGINT: i32 = 130;

/// LLM context content (llms.txt + CUE schemas concatenated at build time)
pub const LLMS_CONTENT: &str = include_str!(concat!(env!("OUT_DIR"), "/llms-full.txt"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cuenv_builder() {
        let cuenv = Cuenv::builder().build();
        // Registry should be empty when no providers are added
        assert!(cuenv.registry.is_empty());
    }

    #[test]
    fn test_cuenv_with_defaults() {
        let cuenv = Cuenv::with_defaults();
        // Should have default sync providers registered
        assert_eq!(cuenv.registry.sync_provider_count(), 3);
    }

    #[test]
    fn test_dynamic_sync_command() {
        let cuenv = Cuenv::with_defaults();
        let sync_cmd = cuenv.build_sync_command();

        // Should have the expected subcommands from providers
        let subcommands: Vec<_> = sync_cmd.get_subcommands().map(|c| c.get_name()).collect();
        assert!(subcommands.contains(&"ci"), "Missing 'ci' subcommand");
        assert!(subcommands.contains(&"cubes"), "Missing 'cubes' subcommand");
        assert!(subcommands.contains(&"rules"), "Missing 'rules' subcommand");
    }

    #[test]
    fn test_dynamic_sync_command_empty_registry() {
        let cuenv = Cuenv::builder().build();
        let sync_cmd = cuenv.build_sync_command();

        // Should have no subcommands when registry is empty
        let subcommand_count = sync_cmd.get_subcommands().count();
        assert_eq!(subcommand_count, 0);
    }

    #[test]
    fn test_run_with_empty_registry() {
        let cuenv = Cuenv::builder().build();
        let result = cuenv.run();
        // Should succeed (placeholder returns OK)
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_defaults() {
        let cuenv = Cuenv::with_defaults();
        let result = cuenv.run();
        // Should succeed (placeholder returns OK)
        assert!(result.is_ok());
    }

    #[test]
    fn test_exit_sigint_constant() {
        // SIGINT exit code is 128 + 2 = 130
        assert_eq!(EXIT_SIGINT, 130);
    }

    #[test]
    fn test_llms_content_not_empty() {
        // LLM content should contain some text
        assert!(!LLMS_CONTENT.is_empty());
    }

    #[test]
    fn test_sync_command_has_path_arg() {
        let cuenv = Cuenv::with_defaults();
        let sync_cmd = cuenv.build_sync_command();

        let args: Vec<_> = sync_cmd.get_arguments().map(|a| a.get_id().as_str()).collect();
        assert!(args.contains(&"path"));
        assert!(args.contains(&"package"));
        assert!(args.contains(&"dry-run"));
        assert!(args.contains(&"check"));
        assert!(args.contains(&"all"));
    }

    #[test]
    fn test_run_cli_with_registry_returns_ok() {
        let exit_code = run_cli_with_registry(Cuenv::builder().build());
        assert_eq!(exit_code, EXIT_OK);
    }
}
