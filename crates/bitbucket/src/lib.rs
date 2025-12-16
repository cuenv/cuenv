//! Bitbucket provider implementations for cuenv.
//!
//! This crate provides Bitbucket-specific implementations of:
//! - [`BitbucketCodeownersProvider`] for CODEOWNERS file management (feature: `codeowners`)
//!
//! # Features
//!
//! - `codeowners` (default): CODEOWNERS file sync and check operations

#![warn(missing_docs)]

#[cfg(feature = "codeowners")]
pub mod codeowners;

// Re-exports for convenience
#[cfg(feature = "codeowners")]
pub use codeowners::BitbucketCodeownersProvider;
