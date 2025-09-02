//! Core types and utilities for cuenv
//!
//! This crate provides enhanced error handling with miette diagnostics,
//! structured error reporting, and contextual information.

use miette::{Diagnostic, SourceSpan};
use std::path::Path;
use thiserror::Error;

/// Main error type for cuenv operations with enhanced diagnostics
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error("Configuration error")]
    #[diagnostic(
        code(cuenv::config::invalid),
        help("Check your cuenv.cue configuration file for syntax errors or invalid values")
    )]
    Configuration {
        #[source_code]
        src: String,
        #[label("invalid configuration")]
        span: Option<SourceSpan>,
        message: String,
    },

    #[error("FFI operation failed")]
    #[diagnostic(code(cuenv::ffi::error))]
    Ffi {
        function: &'static str,
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("CUE parsing failed")]
    #[diagnostic(code(cuenv::cue::parse_error))]
    CueParse {
        path: Box<Path>,
        #[source_code]
        src: Option<String>,
        #[label("parsing failed here")]
        span: Option<SourceSpan>,
        message: String,
        suggestions: Option<Vec<String>>,
    },

    #[error("I/O operation failed")]
    #[diagnostic(
        code(cuenv::io::error),
        help("Check file permissions and ensure the path exists")
    )]
    Io {
        #[source]
        source: std::io::Error,
        path: Option<Box<Path>>,
        operation: String,
    },

    #[error("Text encoding error")]
    #[diagnostic(
        code(cuenv::encoding::utf8),
        help("The file contains invalid UTF-8. Ensure your files use UTF-8 encoding.")
    )]
    Utf8 {
        #[source]
        source: std::str::Utf8Error,
        file: Option<Box<Path>>,
    },

    #[error("Operation timed out after {seconds} seconds")]
    #[diagnostic(
        code(cuenv::timeout),
        help("Try increasing the timeout or check if the operation is stuck")
    )]
    Timeout { seconds: u64 },

    #[error("Validation failed")]
    #[diagnostic(code(cuenv::validation::failed))]
    Validation {
        #[source_code]
        src: Option<String>,
        #[label("validation failed")]
        span: Option<SourceSpan>,
        message: String,
        #[related]
        related: Vec<Error>,
    },
}

impl Error {
    pub fn configuration(msg: impl Into<String>) -> Self {
        Error::Configuration {
            src: String::new(),
            span: None,
            message: msg.into(),
        }
    }

    pub fn configuration_with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        Error::Configuration {
            src: src.into(),
            span,
            message: msg.into(),
        }
    }

    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        Error::Ffi {
            function,
            message: message.into(),
            help: None,
        }
    }

    pub fn ffi_with_help(
        function: &'static str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Error::Ffi {
            function,
            message: message.into(),
            help: Some(help.into()),
        }
    }

    pub fn cue_parse(path: &Path, message: impl Into<String>) -> Self {
        Error::CueParse {
            path: path.into(),
            src: None,
            span: None,
            message: message.into(),
            suggestions: None,
        }
    }

    pub fn cue_parse_with_source(
        path: &Path,
        message: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
        suggestions: Option<Vec<String>>,
    ) -> Self {
        Error::CueParse {
            path: path.into(),
            src: Some(src.into()),
            span,
            message: message.into(),
            suggestions,
        }
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation {
            src: None,
            span: None,
            message: msg.into(),
            related: Vec::new(),
        }
    }

    pub fn validation_with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        Error::Validation {
            src: Some(src.into()),
            span,
            message: msg.into(),
            related: Vec::new(),
        }
    }
}

// Implement conversions for common error types
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            source,
            path: None,
            operation: "unknown".to_string(),
        }
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(source: std::str::Utf8Error) -> Self {
        Error::Utf8 { source, file: None }
    }
}

/// Result type alias for cuenv operations
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration limits
pub struct Limits {
    pub max_path_length: usize,
    pub max_package_name_length: usize,
    pub max_output_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_path_length: 4096,
            max_package_name_length: 256,
            max_output_size: 100 * 1024 * 1024, // 100MB
        }
    }
}
