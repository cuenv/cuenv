//! Core types and utilities for cuenv
//!
//! This crate provides enhanced error handling with miette diagnostics,
//! structured error reporting, and contextual information.
//!
//! ## Type-safe wrappers
//!
//! This crate provides validated newtype wrappers for common domain types:
//!
//! - [`PackageDir`] - A validated directory path that must exist and be a directory
//! - [`PackageName`] - A validated package name following CUE package naming rules
//!
//! ## Examples
//!
//! ```rust
//! use cuenv_core::{PackageDir, PackageName};
//! use std::path::Path;
//!
//! // Validate a directory exists and is actually a directory
//! let pkg_dir = match PackageDir::try_from(Path::new(".")) {
//!     Ok(dir) => dir,
//!     Err(e) => {
//!         eprintln!("Invalid directory: {}", e);
//!         return;
//!     }
//! };
//!
//! // Validate a package name follows naming rules
//! let pkg_name = match PackageName::try_from("my-package") {
//!     Ok(name) => name,
//!     Err(e) => {
//!         eprintln!("Invalid package name: {}", e);
//!         return;
//!     }
//! };
//! ```

pub mod affected;
pub mod base;
pub mod context;
pub use cuenv_manifest::ci;
pub mod contributors;
pub mod cue;
pub mod environment;
pub mod error;
pub mod http;
pub use cuenv_manifest::lockfile;
pub mod manifest;
pub mod module;
pub mod paths;
pub mod rules;
pub mod runtime;
pub mod secrets;
pub mod shell;
pub mod tasks;
pub mod tools;

// Re-export affected detection types
pub use affected::{
    AffectedBy, PatternMatch, UnmatchablePattern, match_pattern, matches_pattern,
    unmatchable_pattern,
};

// Re-export the per-domain error types and the top-level composition
// (see `error` module; RFC-0006 phase 2e)
pub use error::{ConfigError, Error, EvalError, IoError, Result};
pub use secrets::SecretResolutionError;
pub use tasks::TaskError;
pub use tools::ToolError;

// Re-export module types for convenience
pub use module::{
    Instance, InstanceKind, ModuleEvaluation, ModuleEvaluationInput, ModuleEvaluationMetadata,
};

mod types;

pub use types::{
    DryRun, Limits, OutputCapture, PackageDir, PackageDirError, PackageName, PackageNameError,
    VERSION,
};

#[cfg(test)]
pub mod test_utils;

#[cfg(test)]
mod tests;
