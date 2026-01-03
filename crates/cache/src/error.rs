//! Error types for the cache crate

// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
// https://github.com/rust-lang/rust/issues/147648
#![allow(unused_assignments)]

use miette::Diagnostic;
use std::path::Path;
use thiserror::Error;

/// Error type for cache operations
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// I/O error during cache operations
    #[error("I/O {operation} failed{}", path.as_ref().map_or(String::new(), |p| format!(": {}", p.display())))]
    #[diagnostic(
        code(cuenv::cache::io),
        help("Check file permissions and ensure the path exists")
    )]
    Io {
        /// The underlying I/O error
        #[source]
        source: std::io::Error,
        /// Path that caused the error, if available
        path: Option<Box<Path>>,
        /// Operation that failed (e.g., "read", "write", "create")
        operation: String,
    },

    /// Configuration or validation error
    #[error("Cache configuration error: {message}")]
    #[diagnostic(code(cuenv::cache::config))]
    Configuration {
        /// Error message describing the configuration issue
        message: String,
    },

    /// Cache key not found
    #[error("Cache key not found: {key}")]
    #[diagnostic(
        code(cuenv::cache::not_found),
        help("The cache entry may have been evicted or never existed")
    )]
    NotFound {
        /// The cache key that was not found
        key: String,
    },

    /// Serialization error
    #[error("Serialization error: {message}")]
    #[diagnostic(code(cuenv::cache::serialization))]
    Serialization {
        /// Error message describing the serialization issue
        message: String,
    },
}

impl Error {
    /// Create a configuration error
    #[must_use]
    pub fn configuration(msg: impl Into<String>) -> Self {
        Self::Configuration {
            message: msg.into(),
        }
    }

    /// Create an I/O error with path context
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

    /// Create an I/O error without path context
    #[must_use]
    pub fn io_no_path(source: std::io::Error, operation: impl Into<String>) -> Self {
        Self::Io {
            source,
            path: None,
            operation: operation.into(),
        }
    }

    /// Create a not found error
    #[must_use]
    pub fn not_found(key: impl Into<String>) -> Self {
        Self::NotFound { key: key.into() }
    }

    /// Create a serialization error
    #[must_use]
    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Serialization {
            message: msg.into(),
        }
    }
}

/// Result type for cache operations
pub type Result<T> = std::result::Result<T, Error>;
