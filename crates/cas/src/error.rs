//! Error types for the content-addressed store.

// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
// https://github.com/rust-lang/rust/issues/147648
#![allow(unused_assignments)]

use miette::Diagnostic;
use std::path::Path;
use thiserror::Error;

/// Error type for CAS and action cache operations.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// I/O error while reading or writing a blob.
    #[error("I/O {operation} failed{}", path.as_ref().map_or(String::new(), |p| format!(": {}", p.display())))]
    #[diagnostic(
        code(cuenv::cas::io),
        help("Check file permissions and that the cache directory is writable")
    )]
    Io {
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
        /// Path involved in the failing operation, if any.
        path: Option<Box<Path>>,
        /// Short label describing the operation (e.g. "read", "rename").
        operation: String,
    },

    /// Configuration or validation error.
    #[error("CAS configuration error: {message}")]
    #[diagnostic(code(cuenv::cas::config))]
    Configuration {
        /// Human-readable description.
        message: String,
    },

    /// Requested digest was not present in the store.
    #[error("digest not found in CAS: {digest}")]
    #[diagnostic(
        code(cuenv::cas::not_found),
        help("The blob may have been garbage collected or never written")
    )]
    NotFound {
        /// Digest that was looked up.
        digest: String,
    },

    /// Digest mismatch during verification (corruption or wrong digest provided).
    #[error("digest mismatch: expected {expected}, got {actual}")]
    #[diagnostic(code(cuenv::cas::digest_mismatch))]
    DigestMismatch {
        /// The digest that was claimed.
        expected: String,
        /// The digest that was computed from the bytes.
        actual: String,
    },

    /// JSON encode/decode failure for a CAS-persisted message.
    #[error("serialization error: {message}")]
    #[diagnostic(code(cuenv::cas::serialization))]
    Serialization {
        /// Human-readable description.
        message: String,
    },
}

impl Error {
    /// Build a configuration error.
    #[must_use]
    pub fn configuration(msg: impl Into<String>) -> Self {
        Self::Configuration {
            message: msg.into(),
        }
    }

    /// Build an I/O error with path context.
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

    /// Build an I/O error without path context.
    #[must_use]
    pub fn io_no_path(source: std::io::Error, operation: impl Into<String>) -> Self {
        Self::Io {
            source,
            path: None,
            operation: operation.into(),
        }
    }

    /// Build a not-found error.
    #[must_use]
    pub fn not_found(digest: impl Into<String>) -> Self {
        Self::NotFound {
            digest: digest.into(),
        }
    }

    /// Build a digest-mismatch error.
    #[must_use]
    pub fn digest_mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::DigestMismatch {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    /// Build a serialization error.
    #[must_use]
    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Serialization {
            message: msg.into(),
        }
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
