//! GitHub provider implementations for cuenv.
//!
//! This crate provides GitHub-specific implementations of:
//! - [`GitHubCodeownersProvider`] for CODEOWNERS file management (feature: `codeowners`)
//! - [`GitHubCIProvider`] for GitHub Actions CI integration (feature: `ci`)
//! - [`workflow::GitHubActionsEmitter`] for workflow file generation (feature: `workflow`)
//!
//! # Features
//!
//! - `codeowners` (default): CODEOWNERS file sync and check operations
//! - `ci` (default): GitHub Actions CI provider with check runs and PR comments
//! - `workflow` (default): GitHub Actions workflow file generation from IR

#![warn(missing_docs)]

#[cfg(feature = "codeowners")]
pub mod codeowners;

#[cfg(feature = "ci")]
pub mod ci;

#[cfg(feature = "workflow")]
pub mod workflow;

// Re-exports for convenience
#[cfg(feature = "codeowners")]
pub use codeowners::GitHubCodeownersProvider;

#[cfg(feature = "ci")]
pub use ci::GitHubCIProvider;

#[cfg(feature = "workflow")]
pub use workflow::GitHubActionsEmitter;
