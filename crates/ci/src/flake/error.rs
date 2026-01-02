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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_error_constructor() {
        let err = FlakeLockError::parse("invalid JSON");
        assert!(matches!(err, FlakeLockError::ParseError(_)));
        let display = format!("{err}");
        assert!(display.contains("Failed to parse flake.lock"));
        assert!(display.contains("invalid JSON"));
    }

    #[test]
    fn test_io_error_constructor() {
        let err = FlakeLockError::io("/path/to/flake.lock", "permission denied");
        if let FlakeLockError::IoError { path, message } = err {
            assert_eq!(path, PathBuf::from("/path/to/flake.lock"));
            assert_eq!(message, "permission denied");
        } else {
            panic!("Expected IoError");
        }
    }

    #[test]
    fn test_io_error_display() {
        let err = FlakeLockError::io("/project/flake.lock", "file not found");
        let display = format!("{err}");
        assert!(display.contains("/project/flake.lock"));
        assert!(display.contains("file not found"));
    }

    #[test]
    fn test_strict_violation_constructor() {
        let inputs = vec!["nixpkgs".to_string(), "home-manager".to_string()];
        let err = FlakeLockError::strict_violation(inputs);

        if let FlakeLockError::StrictModeViolation { count, inputs } = err {
            assert_eq!(count, 2);
            assert!(inputs.contains(&"nixpkgs".to_string()));
            assert!(inputs.contains(&"home-manager".to_string()));
        } else {
            panic!("Expected StrictModeViolation");
        }
    }

    #[test]
    fn test_strict_violation_display() {
        let inputs = vec!["input1".to_string(), "input2".to_string()];
        let err = FlakeLockError::strict_violation(inputs);
        let display = format!("{err}");
        assert!(display.contains("2 unlocked input(s)"));
        assert!(display.contains("input1"));
        assert!(display.contains("input2"));
    }

    #[test]
    fn test_missing_lock_file_constructor() {
        let err = FlakeLockError::missing("/project/flake.lock");
        if let FlakeLockError::MissingLockFile { path } = err {
            assert_eq!(path, PathBuf::from("/project/flake.lock"));
        } else {
            panic!("Expected MissingLockFile");
        }
    }

    #[test]
    fn test_missing_lock_file_display() {
        let err = FlakeLockError::missing("/my/project/flake.lock");
        let display = format!("{err}");
        assert!(display.contains("No flake.lock file found"));
        assert!(display.contains("/my/project/flake.lock"));
    }

    #[test]
    fn test_error_debug() {
        let err = FlakeLockError::parse("test error");
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("ParseError"));
    }

    #[test]
    fn test_strict_violation_empty_inputs() {
        let err = FlakeLockError::strict_violation(vec![]);
        if let FlakeLockError::StrictModeViolation { count, inputs } = err {
            assert_eq!(count, 0);
            assert!(inputs.is_empty());
        } else {
            panic!("Expected StrictModeViolation");
        }
    }
}
