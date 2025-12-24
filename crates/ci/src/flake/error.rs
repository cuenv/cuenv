//! Error types for flake.lock parsing and purity analysis

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

/// Errors related to flake.lock parsing and purity analysis
#[derive(Debug, Error, Diagnostic)]
pub enum FlakeLockError {
    /// Failed to parse flake.lock JSON
    #[error("Failed to parse flake.lock: {0}")]
    #[diagnostic(
        code(cuenv::ci::flake::parse),
        help("Ensure flake.lock is valid JSON and follows Nix flake.lock schema v7")
    )]
    ParseError(String),

    /// Failed to read flake.lock file
    #[error("Failed to read flake.lock at {path}: {message}")]
    #[diagnostic(
        code(cuenv::ci::flake::io),
        help("Check that flake.lock exists and is readable")
    )]
    IoError {
        /// Path to the flake.lock file
        path: PathBuf,
        /// Error message
        message: String,
    },

    /// Flake purity check failed in strict mode
    #[error("Flake purity check failed with {count} unlocked input(s): {}", inputs.join(", "))]
    #[diagnostic(
        code(cuenv::ci::flake::impure_strict),
        help(
            "In strict mode, all flake inputs must be locked. Run 'nix flake lock' to fix, or use purity_mode: warning/override"
        )
    )]
    StrictModeViolation {
        /// Number of unlocked inputs
        count: usize,
        /// Names of unlocked inputs
        inputs: Vec<String>,
    },

    /// Missing flake.lock file
    #[error("No flake.lock file found at {path}")]
    #[diagnostic(
        code(cuenv::ci::flake::missing),
        help("Run 'nix flake lock' to generate a flake.lock file")
    )]
    MissingLockFile {
        /// Expected path to the flake.lock file
        path: PathBuf,
    },
}

impl FlakeLockError {
    /// Create a parse error
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::ParseError(message.into())
    }

    /// Create an IO error
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self::IoError {
            path: path.into(),
            message: message.into(),
        }
    }

    /// Create a strict mode violation error
    #[must_use]
    pub const fn strict_violation(inputs: Vec<String>) -> Self {
        Self::StrictModeViolation {
            count: inputs.len(),
            inputs,
        }
    }

    /// Create a missing lock file error
    #[must_use]
    pub fn missing(path: impl Into<PathBuf>) -> Self {
        Self::MissingLockFile { path: path.into() }
    }
}
