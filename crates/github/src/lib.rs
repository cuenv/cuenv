//! GitHub provider implementations for cuenv.
//!
//! This crate provides GitHub-specific implementations of:
//! - [`GitHubCodeownersProvider`] for CODEOWNERS file management (feature: `codeowners`)
//! - [`GitHubCIProvider`] for GitHub Actions CI integration (feature: `ci`)
//!
//! # Features
//!
//! - `codeowners` (default): CODEOWNERS file sync and check operations
//! - `ci` (default): GitHub Actions CI provider with check runs and PR comments

#![warn(missing_docs)]

#[cfg(feature = "codeowners")]
pub mod codeowners;

#[cfg(feature = "ci")]
pub mod ci;

// Re-exports for convenience
#[cfg(feature = "codeowners")]
pub use codeowners::GitHubCodeownersProvider;

#[cfg(feature = "ci")]
pub use ci::GitHubCIProvider;
