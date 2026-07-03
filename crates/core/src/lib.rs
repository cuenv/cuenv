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
pub mod sync;
pub mod tasks;
pub mod tools;

// Re-export affected detection types
pub use affected::{AffectedBy, matches_pattern};

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

/// Version of the `cuenv-core` crate (used by task cache metadata)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
pub mod test_utils;

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

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
