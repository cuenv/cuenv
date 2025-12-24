//! GitHub provider implementations for cuenv.
//!
//! This crate provides GitHub-specific implementations of:
//! - [`GitHubCodeOwnersProvider`] for CODEOWNERS file management (feature: `codeowners`)
//! - [`GitHubCIProvider`] for GitHub Actions CI integration (feature: `ci`)
//! - [`workflow::GitHubActionsEmitter`] for workflow file generation (feature: `workflow`)
//! - [`GitHubReleaseBackend`] for GitHub Releases distribution (feature: `release`)
//! - [`GitHubConfigExt`] for GitHub-specific configuration operations
//! - [`stages`] for GitHub-specific stage contributors (feature: `ci`)
//!
//! # Features
//!
//! - `codeowners` (default): CODEOWNERS file sync and check operations
//! - `ci` (default): GitHub Actions CI provider with check runs and PR comments
//! - `workflow` (default): GitHub Actions workflow file generation from IR
//! - `release` (default): Upload artifacts to GitHub Releases

#![warn(missing_docs)]

pub mod config;

#[cfg(feature = "codeowners")]
pub mod codeowners;

#[cfg(feature = "ci")]
pub mod ci;

#[cfg(feature = "ci")]
pub mod stages;

#[cfg(feature = "workflow")]
pub mod workflow;

#[cfg(feature = "release")]
pub mod release;

// Re-exports for convenience
pub use config::GitHubConfigExt;

#[cfg(feature = "codeowners")]
pub use codeowners::GitHubCodeOwnersProvider;

#[cfg(feature = "ci")]
pub use ci::GitHubCIProvider;

#[cfg(feature = "ci")]
pub use stages::{CachixContributor, GhModelsContributor, github_contributors};

#[cfg(feature = "workflow")]
pub use workflow::GitHubActionsEmitter;

#[cfg(feature = "release")]
pub use release::{GitHubReleaseBackend, GitHubReleaseConfig};
