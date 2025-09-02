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

#[cfg(test)]
mod tests {
    use super::*;
    use miette::SourceSpan;
    use std::path::Path;

    #[test]
    fn test_error_configuration() {
        let err = Error::configuration("test message");
        assert_eq!(err.to_string(), "Configuration error");

        if let Error::Configuration { message, .. } = err {
            assert_eq!(message, "test message");
        } else {
            panic!("Expected Configuration error");
        }
    }

    #[test]
    fn test_error_configuration_with_source() {
        let src = "test source code";
        let span = SourceSpan::from(0..4);
        let err = Error::configuration_with_source("config error", src, Some(span));

        if let Error::Configuration {
            src: source,
            span: s,
            message,
        } = err
        {
            assert_eq!(source, "test source code");
            assert_eq!(s, Some(SourceSpan::from(0..4)));
            assert_eq!(message, "config error");
        } else {
            panic!("Expected Configuration error");
        }
    }

    #[test]
    fn test_error_ffi() {
        let err = Error::ffi("test_function", "FFI failed");
        assert_eq!(err.to_string(), "FFI operation failed");

        if let Error::Ffi {
            function,
            message,
            help,
        } = err
        {
            assert_eq!(function, "test_function");
            assert_eq!(message, "FFI failed");
            assert!(help.is_none());
        } else {
            panic!("Expected Ffi error");
        }
    }

    #[test]
    fn test_error_ffi_with_help() {
        let err = Error::ffi_with_help("test_func", "error msg", "try this instead");

        if let Error::Ffi {
            function,
            message,
            help,
        } = err
        {
            assert_eq!(function, "test_func");
            assert_eq!(message, "error msg");
            assert_eq!(help, Some("try this instead".to_string()));
        } else {
            panic!("Expected Ffi error");
        }
    }

    #[test]
    fn test_error_cue_parse() {
        let path = Path::new("/test/path.cue");
        let err = Error::cue_parse(path, "parsing failed");
        assert_eq!(err.to_string(), "CUE parsing failed");

        if let Error::CueParse {
            path: p, message, ..
        } = err
        {
            assert_eq!(p.as_ref(), Path::new("/test/path.cue"));
            assert_eq!(message, "parsing failed");
        } else {
            panic!("Expected CueParse error");
        }
    }

    #[test]
    fn test_error_cue_parse_with_source() {
        let path = Path::new("/test/file.cue");
        let src = "package test";
        let span = SourceSpan::from(0..7);
        let suggestions = vec!["Check syntax".to_string(), "Verify imports".to_string()];

        let err = Error::cue_parse_with_source(
            path,
            "parse error",
            src,
            Some(span),
            Some(suggestions.clone()),
        );

        if let Error::CueParse {
            path: p,
            src: source,
            span: s,
            message,
            suggestions: sugg,
        } = err
        {
            assert_eq!(p.as_ref(), Path::new("/test/file.cue"));
            assert_eq!(source, Some("package test".to_string()));
            assert_eq!(s, Some(SourceSpan::from(0..7)));
            assert_eq!(message, "parse error");
            assert_eq!(sugg, Some(suggestions));
        } else {
            panic!("Expected CueParse error");
        }
    }

    #[test]
    fn test_error_validation() {
        let err = Error::validation("validation failed");
        assert_eq!(err.to_string(), "Validation failed");

        if let Error::Validation {
            message, related, ..
        } = err
        {
            assert_eq!(message, "validation failed");
            assert!(related.is_empty());
        } else {
            panic!("Expected Validation error");
        }
    }

    #[test]
    fn test_error_validation_with_source() {
        let src = "test validation source";
        let span = SourceSpan::from(5..15);
        let err = Error::validation_with_source("validation error", src, Some(span));

        if let Error::Validation {
            src: source,
            span: s,
            message,
            ..
        } = err
        {
            assert_eq!(source, Some("test validation source".to_string()));
            assert_eq!(s, Some(SourceSpan::from(5..15)));
            assert_eq!(message, "validation error");
        } else {
            panic!("Expected Validation error");
        }
    }

    #[test]
    fn test_error_timeout() {
        let err = Error::Timeout { seconds: 30 };
        assert_eq!(err.to_string(), "Operation timed out after 30 seconds");
    }

    #[test]
    fn test_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();

        if let Error::Io { operation, .. } = err {
            assert_eq!(operation, "unknown");
        } else {
            panic!("Expected Io error");
        }
    }

    #[test]
    fn test_error_from_utf8_error() {
        let bytes = vec![0xFF, 0xFE];
        let utf8_err = std::str::from_utf8(&bytes).unwrap_err();
        let err: Error = utf8_err.into();

        assert!(matches!(err, Error::Utf8 { .. }));
    }

    #[test]
    fn test_limits_default() {
        let limits = Limits::default();
        assert_eq!(limits.max_path_length, 4096);
        assert_eq!(limits.max_package_name_length, 256);
        assert_eq!(limits.max_output_size, 100 * 1024 * 1024);
    }

    #[test]
    fn test_result_type_alias() {
        let ok_result: Result<i32> = Ok(42);
        assert!(ok_result.is_ok());
        if let Ok(value) = ok_result {
            assert_eq!(value, 42);
        }

        let err_result: Result<i32> = Err(Error::configuration("test"));
        assert!(err_result.is_err());
    }

    #[test]
    fn test_error_display() {
        let errors = vec![
            (Error::configuration("test"), "Configuration error"),
            (Error::ffi("func", "msg"), "FFI operation failed"),
            (
                Error::cue_parse(Path::new("/test"), "msg"),
                "CUE parsing failed",
            ),
            (Error::validation("msg"), "Validation failed"),
            (
                Error::Timeout { seconds: 10 },
                "Operation timed out after 10 seconds",
            ),
        ];

        for (error, expected) in errors {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn test_error_diagnostic_codes() {
        use miette::Diagnostic;

        let config_err = Error::configuration("test");
        assert_eq!(
            config_err.code().unwrap().to_string(),
            "cuenv::config::invalid"
        );

        let ffi_err = Error::ffi("func", "msg");
        assert_eq!(ffi_err.code().unwrap().to_string(), "cuenv::ffi::error");

        let cue_err = Error::cue_parse(Path::new("/test"), "msg");
        assert_eq!(
            cue_err.code().unwrap().to_string(),
            "cuenv::cue::parse_error"
        );

        let validation_err = Error::validation("msg");
        assert_eq!(
            validation_err.code().unwrap().to_string(),
            "cuenv::validation::failed"
        );

        let timeout_err = Error::Timeout { seconds: 5 };
        assert_eq!(timeout_err.code().unwrap().to_string(), "cuenv::timeout");
    }
}
