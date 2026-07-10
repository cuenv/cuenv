//! Sync command implementation with provider-based architecture.
//!
//! Built-in providers share one internal request and registry. The public CLI
//! shape remains statically defined by clap, so argument parsing and dispatch
//! cannot drift between separate provider APIs.
//!
//! # Architecture
//!
//! Each operation implements the private `SyncProvider` trait. `SyncRegistry`
//! dispatches named and multi-provider requests for the command handler.

pub mod formatters;
pub mod functions;
pub(crate) mod provider;
pub(crate) mod providers;
pub(crate) mod registry;

// Re-export formatter functions for use by the fmt command
pub use formatters::{
    matches_any_pattern, run_cue_formatter, run_go_formatter, run_nix_formatter, run_rust_formatter,
};

// Re-export for external use (e.g., tests)
pub use functions::{execute_sync_ci, execute_sync_ci_workspace, execute_sync_codegen};
pub(crate) use provider::SyncRequest;
pub use provider::{SyncMode, SyncOptions, SyncScope};
pub(crate) use providers::default_registry;
