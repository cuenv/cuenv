//! Error types for CUE evaluation
//!
//! This module provides standalone error types for the cuengine crate,
//! allowing it to be used independently of the cuenv ecosystem.

use std::path::Path;
use thiserror::Error;

/// Errors that can occur during CUE evaluation
#[derive(Error, Debug)]
pub enum CueEngineError {
    /// Configuration error (invalid paths, settings, etc.)
    #[error("Configuration error: {message}")]
    Configuration {
        /// Error message describing the configuration problem
        message: String,
    },

    /// FFI operation failed
    #[error("FFI operation failed in {function}: {message}")]
    Ffi {
        /// Name of the FFI function that failed
        function: &'static str,
        /// Error message from the FFI layer
        message: String,
    },

    /// CUE parsing or evaluation failed
    #[error("CUE parsing failed at {}: {message}", path.display())]
    CueParse {
        /// Path to the CUE file that failed to parse
        path: Box<Path>,
        /// Error message from the CUE parser
        message: String,
    },

    /// Validation error (input/output validation)
    #[error("Validation failed: {message}")]
    Validation {
        /// Error message describing the validation failure
        message: String,
    },

    /// Cache operation error
    #[error("Cache error: {message}")]
    Cache {
        /// Error message describing the cache problem
        message: String,
    },
}

impl CueEngineError {
    /// Create a configuration error
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration {
            message: message.into(),
        }
    }

    /// Create an FFI error
    #[must_use]
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        Self::Ffi {
            function,
            message: message.into(),
        }
    }

    /// Create a CUE parse error
    #[must_use]
    pub fn cue_parse(path: &Path, message: impl Into<String>) -> Self {
        Self::CueParse {
            path: path.into(),
            message: message.into(),
        }
    }

    /// Create a validation error
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// Create a cache error
    #[must_use]
    pub fn cache(message: impl Into<String>) -> Self {
        Self::Cache {
            message: message.into(),
        }
    }
}

/// Result type for CUE evaluation operations
pub type Result<T> = std::result::Result<T, CueEngineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_configuration_error() {
        let err = CueEngineError::configuration("invalid setting");
        assert!(err.to_string().contains("Configuration error"));
        assert!(err.to_string().contains("invalid setting"));
    }

    #[test]
    fn test_ffi_error() {
        let err = CueEngineError::ffi("cue_eval_module", "null pointer");
        assert!(err.to_string().contains("FFI operation failed"));
        assert!(err.to_string().contains("cue_eval_module"));
        assert!(err.to_string().contains("null pointer"));
    }

    #[test]
    fn test_cue_parse_error() {
        let path = Path::new("/some/path");
        let err = CueEngineError::cue_parse(path, "syntax error");
        assert!(err.to_string().contains("CUE parsing failed"));
        assert!(err.to_string().contains("/some/path"));
        assert!(err.to_string().contains("syntax error"));
    }

    #[test]
    fn test_validation_error() {
        let err = CueEngineError::validation("path too long");
        assert!(err.to_string().contains("Validation failed"));
        assert!(err.to_string().contains("path too long"));
    }

    #[test]
    fn test_cache_error() {
        let err = CueEngineError::cache("capacity must be non-zero");
        assert!(err.to_string().contains("Cache error"));
        assert!(err.to_string().contains("capacity must be non-zero"));
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CueEngineError>();
    }
}
