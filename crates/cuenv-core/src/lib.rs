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

pub mod environment;
pub mod hooks;
pub mod task;
pub mod task_executor;
pub mod task_graph;

#[cfg(test)]
mod environment_test;

use miette::{Diagnostic, SourceSpan};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

/// A validated directory path that must exist and be a directory
///
/// This newtype wrapper ensures that any instance represents a path that:
/// - Exists on the filesystem
/// - Is actually a directory (not a file or symlink to file)
/// - Can be accessed for metadata reading
///
/// # Examples
///
/// ```rust
/// use cuenv_core::PackageDir;
/// use std::path::Path;
///
/// // Try to create from current directory
/// match PackageDir::try_from(Path::new(".")) {
///     Ok(dir) => println!("Valid directory: {}", dir.as_path().display()),
///     Err(e) => eprintln!("Invalid directory: {}", e),
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PackageDir(PathBuf);

impl PackageDir {
    /// Get the path as a reference
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Convert into the underlying PathBuf
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for PackageDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// Errors that can occur when validating a PackageDir
#[derive(Error, Debug, Clone, Diagnostic)]
pub enum PackageDirError {
    /// The path does not exist
    #[error("path does not exist: {0}")]
    #[diagnostic(
        code(cuenv::package_dir::not_found),
        help("Make sure the directory exists and you have permission to access it")
    )]
    NotFound(String),

    /// The path exists but is not a directory
    #[error("path is not a directory: {0}")]
    #[diagnostic(
        code(cuenv::package_dir::not_directory),
        help("The path must point to a directory, not a file")
    )]
    NotADirectory(String),

    /// An I/O error occurred while checking the path
    #[error("io error accessing path: {0}")]
    #[diagnostic(
        code(cuenv::package_dir::io_error),
        help("Check file permissions and ensure you have access to the path")
    )]
    Io(String),
}

impl TryFrom<&Path> for PackageDir {
    type Error = PackageDirError;

    /// Try to create a PackageDir from a path
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cuenv_core::PackageDir;
    /// use std::path::Path;
    ///
    /// match PackageDir::try_from(Path::new(".")) {
    ///     Ok(dir) => println!("Valid directory"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    fn try_from(input: &Path) -> std::result::Result<Self, Self::Error> {
        match std::fs::metadata(input) {
            Ok(meta) => {
                if meta.is_dir() {
                    Ok(PackageDir(input.to_path_buf()))
                } else {
                    Err(PackageDirError::NotADirectory(input.display().to_string()))
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Err(PackageDirError::NotFound(input.display().to_string()))
                } else {
                    Err(PackageDirError::Io(e.to_string()))
                }
            }
        }
    }
}

/// A validated CUE package name
///
/// Package names must follow CUE naming conventions:
/// - 1-64 characters in length
/// - Start with alphanumeric character (A-Z, a-z, 0-9)
/// - Contain only alphanumeric, hyphen (-), or underscore (_) characters
///
/// # Examples
///
/// ```rust
/// use cuenv_core::PackageName;
///
/// // Valid package names
/// assert!(PackageName::try_from("my-package").is_ok());
/// assert!(PackageName::try_from("package_123").is_ok());
/// assert!(PackageName::try_from("app").is_ok());
///
/// // Invalid package names
/// assert!(PackageName::try_from("-invalid").is_err());  // starts with hyphen
/// assert!(PackageName::try_from("invalid.name").is_err());  // contains dot
/// assert!(PackageName::try_from("").is_err());  // empty
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PackageName(String);

impl PackageName {
    /// Get the package name as a string slice
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert into the underlying String
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Errors that can occur when validating a PackageName
#[derive(Error, Debug, Clone, Diagnostic)]
pub enum PackageNameError {
    /// The package name is invalid
    #[error("invalid package name: {0}")]
    #[diagnostic(
        code(cuenv::package_name::invalid),
        help(
            "Package names must be 1-64 characters, start with alphanumeric, and contain only alphanumeric, hyphen, or underscore characters"
        )
    )]
    Invalid(String),
}

impl TryFrom<&str> for PackageName {
    type Error = PackageNameError;

    /// Try to create a PackageName from a string
    ///
    /// # Examples
    ///
    /// ```rust
    /// use cuenv_core::PackageName;
    ///
    /// match PackageName::try_from("my-package") {
    ///     Ok(name) => println!("Valid package name: {}", name),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        let bytes = s.as_bytes();

        // Check length bounds
        if bytes.is_empty() || bytes.len() > 64 {
            return Err(PackageNameError::Invalid(s.to_string()));
        }

        // Check first character must be alphanumeric
        let first = bytes[0];
        let is_alnum =
            |b: u8| b.is_ascii_uppercase() || b.is_ascii_lowercase() || b.is_ascii_digit();

        if !is_alnum(first) {
            return Err(PackageNameError::Invalid(s.to_string()));
        }

        // Check all characters are valid
        let valid = |b: u8| is_alnum(b) || b == b'-' || b == b'_';
        for &b in bytes {
            if !valid(b) {
                return Err(PackageNameError::Invalid(s.to_string()));
            }
        }

        Ok(PackageName(s.to_string()))
    }
}

impl TryFrom<String> for PackageName {
    type Error = PackageNameError;

    /// Try to create a PackageName from an owned String
    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        Self::try_from(s.as_str())
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

    #[test]
    fn test_package_dir_validation() {
        // Current directory should be valid
        let result = PackageDir::try_from(Path::new("."));
        assert!(result.is_ok(), "Current directory should be valid");

        // Get methods should work
        let pkg_dir = result.unwrap();
        assert_eq!(pkg_dir.as_path(), Path::new("."));
        assert_eq!(pkg_dir.as_ref(), Path::new("."));
        assert_eq!(pkg_dir.into_path_buf(), PathBuf::from("."));

        // Non-existent directory should fail with NotFound
        let result = PackageDir::try_from(Path::new("/path/does/not/exist"));
        assert!(result.is_err());
        match result.unwrap_err() {
            PackageDirError::NotFound(_) => {} // Expected
            other => panic!("Expected NotFound error, got: {:?}", other),
        }

        // Path to a file should fail with NotADirectory
        // Create a temporary file
        let temp_path = std::env::temp_dir().join("cuenv_test_file");
        let file = std::fs::File::create(&temp_path).unwrap();
        drop(file);

        let result = PackageDir::try_from(temp_path.as_path());
        assert!(result.is_err());
        match result.unwrap_err() {
            PackageDirError::NotADirectory(_) => {} // Expected
            other => panic!("Expected NotADirectory error, got: {:?}", other),
        }

        // Clean up
        std::fs::remove_file(temp_path).ok();
    }

    #[test]
    fn test_package_name_validation() {
        // Valid package names
        let max_len_string = "a".repeat(64);
        let valid_names = vec![
            "my-package",
            "package_123",
            "a",        // Single character
            "A",        // Uppercase
            "0package", // Starts with number
            "package-with-hyphens",
            "package_with_underscores",
            max_len_string.as_str(), // Max length
        ];

        for name in valid_names {
            let result = PackageName::try_from(name);
            assert!(result.is_ok(), "'{}' should be valid", name);

            // Test the String variant too
            let result = PackageName::try_from(name.to_string());
            assert!(result.is_ok(), "'{}' as String should be valid", name);

            // Verify methods work correctly
            let pkg_name = result.unwrap();
            assert_eq!(pkg_name.as_str(), name);
            assert_eq!(pkg_name.as_ref(), name);
            assert_eq!(pkg_name.to_string(), name);
            assert_eq!(pkg_name.into_string(), name.to_string());
        }

        // Invalid package names
        let too_long_string = "a".repeat(65);
        let invalid_names = vec![
            "",                       // Empty
            "-invalid",               // Starts with hyphen
            "_invalid",               // Starts with underscore
            "invalid.name",           // Contains dot
            "invalid/name",           // Contains slash
            "invalid:name",           // Contains colon
            too_long_string.as_str(), // Too long
            "invalid@name",           // Contains @
            "invalid#name",           // Contains #
            "invalid name",           // Contains space
        ];

        for name in invalid_names {
            let result = PackageName::try_from(name);
            assert!(result.is_err(), "'{}' should be invalid", name);

            // Verify error type is correct
            assert!(matches!(result.unwrap_err(), PackageNameError::Invalid(_)));
        }
    }
}
