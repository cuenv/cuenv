//! Sync command implementation with provider-based architecture.
//!
//! This module provides a trait-based system for syncing different types of
//! generated files from CUE configuration:
//! - Cube-generated files
//! - CI workflow files
//! - Rules files (ignore, editorconfig, codeowners via .rules.cue)
//!
//! # Architecture
//!
//! The sync system uses a provider pattern where each type of sync operation
//! implements the `SyncProvider` trait. Providers are registered with a
//! `SyncRegistry` which handles collective operations like `cuenv sync -A`.
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv::commands::sync::{default_registry, SyncOptions};
//!
//! let registry = default_registry();
//! let options = SyncOptions::default();
//!
//! // Sync all providers
//! registry.sync_all(&path, "cuenv", &options, true, &executor).await?;
//!
//! // Sync specific provider
//! registry.sync_provider("cubes", &path, "cuenv", &options, true, &executor).await?;
//! ```

pub mod formatters;
pub mod functions;
pub mod provider;
pub mod providers;
pub mod registry;

// Re-export for external use (e.g., tests)
#[allow(unused_imports)]
pub use functions::{execute_sync_ci, execute_sync_ci_workspace, execute_sync_cubes};
pub use provider::{SyncMode, SyncOptions};
pub use providers::default_registry;

// Re-export for extensibility
#[allow(unused_imports)]
pub use provider::{SyncProvider, SyncResult};
#[allow(unused_imports)]
pub use registry::SyncRegistry;
