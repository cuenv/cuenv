//! Homebrew tap provider for cuenv.
//!
//! This crate provides the [`HomebrewBackend`] for publishing release artifacts
//! to a Homebrew tap repository.
//!
//! # Features
//!
//! - Formula generation from release artifacts
//! - Automatic push to tap repository via GitHub API
//! - Support for multi-platform binaries (macOS, Linux)
//!
//! # Example
//!
//! ```rust,ignore
//! use cuenv_homebrew::{HomebrewBackend, HomebrewConfig};
//! use cuenv_release::backends::BackendContext;
//!
//! let config = HomebrewConfig::new("cuenv/homebrew-tap", "cuenv")
//!     .with_homepage("https://github.com/cuenv/cuenv")
//!     .with_license("AGPL-3.0-or-later");
//!
//! let backend = HomebrewBackend::new(config);
//! ```

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

mod backend;
mod formula;

pub use backend::{HomebrewBackend, HomebrewConfig};
pub use formula::{BinaryInfo, FormulaData, FormulaGenerator};
