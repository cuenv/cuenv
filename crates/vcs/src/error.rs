//! Errors produced by the [`VcsHasher`](crate::VcsHasher) implementations.

// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
#![allow(unused_assignments)]

use miette::Diagnostic;
use std::path::Path;
use thiserror::Error;

/// Errors produced while resolving or hashing input files.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// An I/O operation failed.
    #[error("I/O {operation} failed{}", path.as_ref().map_or(String::new(), |p| format!(": {}", p.display())))]
    #[diagnostic(
        code(cuenv::vcs::io),
        help("Check that the path exists and that cuenv has permission to read it")
    )]
    Io {
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
        /// Path that triggered the failure, if known.
        path: Option<Box<Path>>,
        /// Operation being attempted (e.g. `read`, `open`).
        operation: String,
    },

    /// A supplied glob/pattern was invalid.
    #[error("invalid input pattern: {message}")]
    #[diagnostic(
        code(cuenv::vcs::pattern),
        help("Patterns are globs rooted at the workspace (e.g. `src/**/*.rs`)")
    )]
    Pattern {
        /// Human-readable explanation.
        message: String,
    },
}

impl Error {
    /// Build an I/O error tagged with a path and operation.
    #[must_use]
    pub fn io(
        source: std::io::Error,
        path: impl AsRef<Path>,
        operation: impl Into<String>,
    ) -> Self {
        Self::Io {
            source,
            path: Some(path.as_ref().into()),
            operation: operation.into(),
        }
    }

    /// Build a pattern error.
    #[must_use]
    pub fn pattern(message: impl Into<String>) -> Self {
        Self::Pattern {
            message: message.into(),
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
