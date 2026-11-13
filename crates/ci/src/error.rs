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

/// A CI operation using the shared core error type.
pub type Result<T> = std::result::Result<T, cuenv_core::Error>;

/// Error types for CI execution
#[derive(Debug, Error)]
#[non_exhaustive]
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

    /// Task graph could not be ordered (for example, a dependency cycle)
    #[error(transparent)]
    TaskGraph(#[from] cuenv_task_graph::Error),

    /// Task panicked during execution
    #[error("Task panicked: {0}")]
    TaskPanic(String),

    /// Pipeline not found
    #[error("Pipeline '{name}' not found. Available: {available}")]
    PipelineNotFound {
        /// Requested pipeline name.
        name: String,
        /// Names of the configured pipelines.
        available: String,
    },

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
            ExecutorError::TaskGraph(e) => Self::from(e),
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
#[non_exhaustive]
pub enum RunnerError {
    /// Task command is empty
    #[error("Task '{task}' has empty command")]
    EmptyCommand {
        /// Task whose command is empty.
        task: String,
    },

    /// Process spawn failed
    #[error("Failed to spawn task '{task}': {source}")]
    SpawnFailed {
        /// Task whose process could not be started.
        task: String,
        /// Operating system error returned when starting the process.
        #[source]
        source: std::io::Error,
    },

    /// Process execution failed
    #[error("Task '{task}' execution failed: {source}")]
    ExecutionFailed {
        /// Task whose process could not be observed to completion.
        task: String,
        /// Operating system error returned while running the process.
        #[source]
        source: std::io::Error,
    },
}

/// Compiler errors
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompilerError {
    /// The compiled task graph did not pass validation.
    #[error("Task graph validation failed: {0}")]
    ValidationFailed(String),

    /// A requested task could not be resolved in the project.
    #[error("Task '{0}' not found")]
    TaskNotFound(String),

    /// A shell script cannot be represented as the requested command array.
    #[error("Task '{0}' uses shell script but IR requires command array")]
    ShellScriptNotSupported(String),

    /// A task declaration cannot be lowered to the CI representation.
    #[error("Invalid task structure: {0}")]
    InvalidTaskStructure(String),

    /// The pipeline reaches a node that cannot run: a problem recorded
    /// against it when the project's value was read, or a file reference
    /// that does not bind.
    #[error("Pipeline '{pipeline}' reaches what cannot run:\n  {}", problems.join("\n  "))]
    UnrunnableNodes {
        /// The pipeline being compiled.
        pipeline: String,
        /// One line per problem, naming the node.
        problems: Vec<String>,
    },

    /// Flake lock analysis failed while compiling a runtime.
    #[error("Flake lock error: {0}")]
    FlakeLock(#[from] FlakeLockError),
}

/// Errors related to flake.lock parsing and purity analysis
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
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
#[non_exhaustive]
pub enum ValidationError {
    /// The task dependency graph contains a cycle.
    #[error("Task graph contains cycle: {0}")]
    CyclicDependency(String),

    /// A task refers to another task absent from the pipeline.
    #[error("Task '{task}' depends on non-existent task '{dependency}'")]
    MissingDependency {
        /// Task with the unresolved dependency.
        task: String,
        /// Referenced task name.
        dependency: String,
    },

    /// A task refers to a runtime absent from the pipeline.
    #[error("Task '{task}' references non-existent runtime '{runtime}'")]
    MissingRuntime {
        /// Task with the unresolved runtime.
        task: String,
        /// Referenced runtime identifier.
        runtime: String,
    },

    /// A deployment task is followed by a non-deployment dependent.
    #[error("Deployment task '{deployment}' has non-deployment dependent '{dependent}'")]
    InvalidDeploymentDependency {
        /// Deployment task in the invalid dependency edge.
        deployment: String,
        /// Non-deployment task that depends on it.
        dependent: String,
    },

    /// A non-shell task uses a string command instead of an argument array.
    #[error("Task '{task}' has shell=false with string command (must be array)")]
    InvalidShellCommand {
        /// Task with the invalid command representation.
        task: String,
    },

    /// A task has no command to execute.
    #[error("Task '{task}' has empty command")]
    EmptyCommand {
        /// Task with the empty command.
        task: String,
    },

    /// A deployment task enables a cache policy.
    #[error("Deployment task '{task}' has cache_policy={policy:?} (must be disabled)")]
    InvalidDeploymentCachePolicy {
        /// Deployment task with the invalid policy.
        task: String,
        /// Policy specified for the task.
        policy: crate::ir::CachePolicy,
    },

    /// A declared task input was not found during compilation.
    #[error("Task '{task}' declares input '{input}' that does not exist at compile time")]
    MissingInput {
        /// Task declaring the missing input.
        task: String,
        /// Input path or pattern that could not be found.
        input: String,
    },
}

/// Error types for emitter operations
#[derive(Debug, Error)]
#[non_exhaustive]
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
        /// Feature the emitter cannot represent.
        feature: String,
        /// Emitter that rejected the feature.
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
#[non_exhaustive]
pub enum DiffError {
    /// Report file not found
    #[error("Report not found: {0}")]
    ReportNotFound(PathBuf),

    /// Failed to read report
    #[error("Failed to read report '{path}': {source}")]
    ReadError {
        /// Report path that could not be read.
        path: PathBuf,
        /// Operating system error returned while reading the report.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse report
    #[error("Failed to parse report '{path}': {source}")]
    ParseError {
        /// Report path containing invalid JSON.
        path: PathBuf,
        /// JSON parsing error.
        #[source]
        source: serde_json::Error,
    },

    /// Invalid run identifier
    #[error("Invalid run identifier: {0}")]
    InvalidRunId(String),
}

/// Errors for garbage collection
#[derive(Debug, Error)]
#[non_exhaustive]
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
