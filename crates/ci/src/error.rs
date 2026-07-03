//! Colocated error types for cuenv-ci (RFC-0006 phase 2e).
//!
//! Each pipeline stage keeps its own dedicated error enum — compilation,
//! validation, emission, execution, and maintenance fail in genuinely
//! different ways and are handled by different callers. They are colocated
//! here so the crate's error surface is visible in one place, but they are
//! deliberately NOT merged into a unified `CiError` mega-enum: no consumer
//! matches across stages, and a single catch-all enum would recreate the
//! god-error shape this phase removed from cuenv-core. The crate-level
//! `Result` remains an alias for `cuenv_core::Error`; `ExecutorError` is the
//! only type bridged into it.
//!
//! The defining modules re-export their error type (e.g.
//! `crate::executor::ExecutorError`), so existing paths keep working.

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

/// Error types for CI execution
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// Compilation error
    #[error("Failed to compile project to IR: {0}")]
    Compilation(String),

    /// Secret resolution error
    #[error(transparent)]
    Secret(#[from] cuenv_secrets::SecretError),

    /// Task execution error
    #[error(transparent)]
    Runner(#[from] RunnerError),

    /// Task panicked during execution
    #[error("Task panicked: {0}")]
    TaskPanic(String),

    /// Pipeline not found
    #[error("Pipeline '{name}' not found. Available: {available}")]
    PipelineNotFound { name: String, available: String },

    /// No CI configuration
    #[error("Project has no CI configuration")]
    NoCIConfig,
}

impl From<ExecutorError> for cuenv_core::Error {
    fn from(err: ExecutorError) -> Self {
        match err {
            ExecutorError::Compilation(msg) => Self::configuration(msg),
            ExecutorError::Secret(e) => Self::secret_resolution(e.to_string()),
            ExecutorError::Runner(e) => Self::execution(e.to_string()),
            ExecutorError::TaskPanic(msg) => Self::execution(format!("Task panicked: {msg}")),
            ExecutorError::PipelineNotFound { name, available } => Self::configuration(format!(
                "Pipeline '{name}' not found. Available: {available}"
            )),
            ExecutorError::NoCIConfig => Self::configuration("Project has no CI configuration"),
        }
    }
}

/// Error types for task execution
#[derive(Debug, Error)]
pub enum RunnerError {
    /// Task command is empty
    #[error("Task '{task}' has empty command")]
    EmptyCommand { task: String },

    /// Process spawn failed
    #[error("Failed to spawn task '{task}': {source}")]
    SpawnFailed {
        task: String,
        #[source]
        source: std::io::Error,
    },

    /// Process execution failed
    #[error("Task '{task}' execution failed: {source}")]
    ExecutionFailed {
        task: String,
        #[source]
        source: std::io::Error,
    },
}

/// Compiler errors
#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("Task graph validation failed: {0}")]
    ValidationFailed(String),

    #[error("Task '{0}' not found")]
    TaskNotFound(String),

    #[error("Task '{0}' uses shell script but IR requires command array")]
    ShellScriptNotSupported(String),

    #[error("Invalid task structure: {0}")]
    InvalidTaskStructure(String),

    #[error("Flake lock error: {0}")]
    FlakeLock(#[from] FlakeLockError),
}

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

/// Validation errors for IR documents
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Task graph contains cycle: {0}")]
    CyclicDependency(String),

    #[error("Task '{task}' depends on non-existent task '{dependency}'")]
    MissingDependency { task: String, dependency: String },

    #[error("Task '{task}' references non-existent runtime '{runtime}'")]
    MissingRuntime { task: String, runtime: String },

    #[error("Deployment task '{deployment}' has non-deployment dependent '{dependent}'")]
    InvalidDeploymentDependency {
        deployment: String,
        dependent: String,
    },

    #[error("Task '{task}' has shell=false with string command (must be array)")]
    InvalidShellCommand { task: String },

    #[error("Task '{task}' has empty command")]
    EmptyCommand { task: String },

    #[error("Deployment task '{task}' has cache_policy={policy:?} (must be disabled)")]
    InvalidDeploymentCachePolicy {
        task: String,
        policy: crate::ir::CachePolicy,
    },

    #[error("Task '{task}' declares input '{input}' that does not exist at compile time")]
    MissingInput { task: String, input: String },
}

/// Error types for emitter operations
#[derive(Debug, Error)]
pub enum EmitterError {
    /// YAML/JSON serialization failed
    #[error("Serialization failed: {0}")]
    Serialization(String),

    /// Invalid IR structure for this emitter
    #[error("Invalid IR: {0}")]
    InvalidIR(String),

    /// Unsupported feature in IR for this emitter
    #[error("Unsupported feature '{feature}' for {emitter} emitter")]
    UnsupportedFeature {
        feature: String,
        emitter: &'static str,
    },

    /// IO error during emission
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for emitter operations
pub type EmitterResult<T> = std::result::Result<T, EmitterError>;

/// Errors for diff operations
#[derive(Debug, Error)]
pub enum DiffError {
    /// Report file not found
    #[error("Report not found: {0}")]
    ReportNotFound(PathBuf),

    /// Failed to read report
    #[error("Failed to read report '{path}': {source}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse report
    #[error("Failed to parse report '{path}': {source}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Invalid run identifier
    #[error("Invalid run identifier: {0}")]
    InvalidRunId(String),
}

/// Errors for garbage collection
#[derive(Debug, Error)]
pub enum GCError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Cache directory not found
    #[error("Cache directory not found: {0}")]
    CacheDirNotFound(PathBuf),

    /// Nix garbage collection failed
    #[error("Nix garbage collection failed: {0}")]
    NixGCFailed(String),
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
