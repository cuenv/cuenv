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

// Rust 1.92 compiler bug: false positives for thiserror/miette derive macro fields
// https://github.com/rust-lang/rust/issues/147648
#![allow(unused_assignments)]

pub mod affected;
pub mod base;
pub mod ci;
pub mod config;
pub mod contributors;
pub mod cue;
pub mod environment;
pub mod http;
pub mod lockfile;
pub mod manifest;
pub mod module;
pub mod owners;
pub mod paths;
pub mod rules;
pub mod runtime;
pub mod secrets;
pub mod shell;
pub mod sync;
pub mod tasks;
pub mod tools;

// Re-export affected detection types
pub use affected::{AffectedBy, matches_pattern};

// Re-export module types for convenience
pub use module::{
    Instance, InstanceKind, ModuleEvaluation, ModuleEvaluationInput, ModuleEvaluationMetadata,
};

/// Version of the `cuenv-core` crate (used by task cache metadata)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
pub mod test_utils;

use miette::{Diagnostic, SourceSpan};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Main error type for cuenv operations with enhanced diagnostics
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error("Configuration error: {message}")]
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

    #[error("FFI operation failed in {function}: {message}")]
    #[diagnostic(code(cuenv::ffi::error))]
    Ffi {
        function: &'static str,
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("CUE parsing failed: {message}")]
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

    #[error("I/O {operation} failed{}", path.as_ref().map_or(String::new(), |p| format!(": {}", p.display())))]
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

    #[error("Validation failed: {message}")]
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

    #[error("Task execution failed: {message}")]
    #[diagnostic(code(cuenv::task::execution))]
    Execution {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Tool resolution failed: {message}")]
    #[diagnostic(code(cuenv::tool::resolution))]
    ToolResolution {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Platform error: {message}")]
    #[diagnostic(
        code(cuenv::platform::error),
        help("This platform may not be supported by the tool provider")
    )]
    Platform { message: String },

    #[error("Task '{task_name}' failed with exit code {exit_code}")]
    #[diagnostic(code(cuenv::task::failed))]
    TaskFailed {
        task_name: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        #[help]
        help: Option<String>,
    },

    #[error("Task graph error: {message}")]
    #[diagnostic(code(cuenv::task::graph))]
    TaskGraph {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Secret resolution failed: {message}")]
    #[diagnostic(code(cuenv::secret::resolution))]
    SecretResolution {
        message: String,
        #[help]
        help: Option<String>,
    },
}

impl Error {
    #[must_use]
    pub fn configuration(msg: impl Into<String>) -> Self {
        Error::Configuration {
            src: String::new(),
            span: None,
            message: msg.into(),
        }
    }

    #[must_use]
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

    #[must_use]
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        Error::Ffi {
            function,
            message: message.into(),
            help: None,
        }
    }

    #[must_use]
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

    #[must_use]
    pub fn cue_parse(path: &Path, message: impl Into<String>) -> Self {
        Error::CueParse {
            path: path.into(),
            src: None,
            span: None,
            message: message.into(),
            suggestions: None,
        }
    }

    #[must_use]
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

    #[must_use]
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation {
            src: None,
            span: None,
            message: msg.into(),
            related: Vec::new(),
        }
    }

    #[must_use]
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

    #[must_use]
    pub fn execution(msg: impl Into<String>) -> Self {
        Error::Execution {
            message: msg.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn execution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        Error::Execution {
            message: msg.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn tool_resolution(msg: impl Into<String>) -> Self {
        Error::ToolResolution {
            message: msg.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn tool_resolution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        Error::ToolResolution {
            message: msg.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn platform(msg: impl Into<String>) -> Self {
        Error::Platform {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn task_failed(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Error::TaskFailed {
            task_name: task_name.into(),
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn task_failed_with_help(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Error::TaskFailed {
            task_name: task_name.into(),
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn task_graph(message: impl Into<String>) -> Self {
        Error::TaskGraph {
            message: message.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn task_graph_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Error::TaskGraph {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn secret_resolution(message: impl Into<String>) -> Self {
        Error::SecretResolution {
            message: message.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn secret_resolution_with_help(
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Error::SecretResolution {
            message: message.into(),
            help: Some(help.into()),
        }
    }
}

// Implement conversions for common error types
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            source,
            path: None,
            operation: "unknown (unmapped error conversion)".to_string(),
        }
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(source: std::str::Utf8Error) -> Self {
        Error::Utf8 { source, file: None }
    }
}

impl From<cuenv_hooks::Error> for Error {
    fn from(source: cuenv_hooks::Error) -> Self {
        Error::Execution {
            message: source.to_string(),
            help: None,
        }
    }
}

impl From<cuenv_task_graph::Error> for Error {
    fn from(err: cuenv_task_graph::Error) -> Self {
        let help = match &err {
            cuenv_task_graph::Error::CycleDetected { .. } => {
                Some("Check for circular dependencies between tasks".into())
            }
            cuenv_task_graph::Error::MissingDependency { task, dependency } => Some(format!(
                "Add task '{}' or remove it from {}'s dependsOn",
                dependency, task
            )),
            cuenv_task_graph::Error::MissingDependencies { missing } => {
                let suggestions: Vec<String> = missing
                    .iter()
                    .map(|(task, dep)| {
                        format!("  - Add '{}' or remove from {}'s dependsOn", dep, task)
                    })
                    .collect();
                Some(format!(
                    "Fix missing dependencies:\n{}",
                    suggestions.join("\n")
                ))
            }
            cuenv_task_graph::Error::TopologicalSortFailed { .. } => None,
            cuenv_task_graph::Error::DuplicateNodeName {
                name,
                existing_kind,
                new_kind,
            } => Some(format!(
                "Rename the {new_kind} '{name}' to avoid collision with the existing {existing_kind}"
            )),
        };
        Error::TaskGraph {
            message: err.to_string(),
            help,
        }
    }
}

/// Result type alias for cuenv operations
pub type Result<T> = std::result::Result<T, Error>;

/// Type-safe replacement for `dry_run: bool` function parameters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum DryRun {
    /// Perform the operation for real
    #[default]
    No,
    /// Preview the operation without making changes
    Yes,
}

impl DryRun {
    /// Returns `true` if this is a dry run.
    #[must_use]
    pub const fn is_dry_run(self) -> bool {
        matches!(self, Self::Yes)
    }
}

impl From<bool> for DryRun {
    fn from(v: bool) -> Self {
        if v { Self::Yes } else { Self::No }
    }
}

/// Type-safe replacement for `capture_output: bool` function parameters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum OutputCapture {
    /// Capture stdout/stderr into buffers
    #[default]
    Capture,
    /// Stream stdout/stderr to the terminal
    Stream,
}

impl OutputCapture {
    /// Returns `true` if output should be captured.
    #[must_use]
    pub const fn should_capture(self) -> bool {
        matches!(self, Self::Capture)
    }
}

impl From<bool> for OutputCapture {
    fn from(v: bool) -> Self {
        if v { Self::Capture } else { Self::Stream }
    }
}

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
mod tests;
