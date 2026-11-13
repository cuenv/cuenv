//! Error types for cuenv operations.
//!
//! Errors are decomposed per domain (RFC-0006 phase 2e): each domain owns a
//! self-contained `thiserror` + `miette` error type colocated with its module
//! ([`ConfigError`], [`EvalError`], [`IoError`] here; [`TaskError`] in
//! `tasks::error`; [`ToolError`] in `tools::error`;
//! [`SecretResolutionError`] in `secrets::error`), and the top-level
//! [`Error`] is a thin transparent composition of them. Display output,
//! diagnostic codes, and help text are forwarded unchanged, so rendered
//! diagnostics are identical to the former monolithic enum.
//!
//! Construction goes through the [`Error`] helper constructors
//! (`Error::configuration(...)`, `Error::io(...)`, ...) which delegate to the
//! domain constructors; both layers are public so extracted crates can build
//! domain errors directly.

use crate::secrets::SecretResolutionError;
use crate::tasks::TaskError;
use crate::tools::ToolError;
use miette::{Diagnostic, SourceSpan};
use std::path::Path;
use thiserror::Error as ThisError;

/// Source context for a CUE parse diagnostic.
#[derive(Debug, Clone)]
pub struct CueParseSource {
    /// CUE source text.
    pub text: String,
    /// Highlighted location, if known.
    pub span: Option<SourceSpan>,
    /// Suggested remedies, if available.
    pub suggestions: Option<Vec<String>>,
}

/// Configuration-domain error.
///
/// Deliberately still the catch-all shape used by ~10 core domains;
/// construction sites migrate to dedicated domain errors incrementally as
/// those domains are extracted (RFC-0006 phases 3+).
#[derive(ThisError, Debug, Diagnostic)]
#[error("Configuration error: {message}")]
#[diagnostic(
    code(cuenv::config::invalid),
    help("Check your cuenv.cue configuration file for syntax errors or invalid values")
)]
pub struct ConfigError {
    /// Configuration source text the diagnostic points into; empty when no
    /// source is attached.
    #[source_code]
    pub src: String,
    /// Location of the invalid configuration within `src`, if known.
    #[label("invalid configuration")]
    pub span: Option<SourceSpan>,
    /// Human-readable description of what is wrong with the configuration.
    pub message: String,
}

impl ConfigError {
    /// Creates a configuration error with a message and no source context.
    #[must_use]
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            src: String::new(),
            span: None,
            message: msg.into(),
        }
    }

    /// Creates a configuration error that highlights `span` within the
    /// configuration source `src`.
    #[must_use]
    pub fn with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        Self {
            src: src.into(),
            span,
            message: msg.into(),
        }
    }
}

/// CUE evaluation domain errors: FFI bridge failures, CUE parsing, and
/// schema/config validation.
#[derive(ThisError, Debug, Diagnostic)]
#[non_exhaustive]
pub enum EvalError {
    /// A call across the Go FFI bridge into the CUE evaluator failed.
    #[error("FFI operation failed in {function}: {message}")]
    #[diagnostic(code(cuenv::ffi::error))]
    Ffi {
        /// Name of the bridge function that failed.
        function: &'static str,
        /// Failure reported by the bridge.
        message: String,
        /// Suggested remedy shown with the diagnostic, if any.
        #[help]
        help: Option<String>,
    },

    /// CUE failed to parse or evaluate the configuration at `path`.
    #[error("CUE evaluation failed: {message}")]
    #[diagnostic(code(cuenv::cue::parse_error))]
    CueParse {
        /// Path of the CUE file or directory that was being evaluated.
        path: Box<Path>,
        /// CUE source text the diagnostic points into, if available.
        #[source_code]
        src: Option<String>,
        /// Location of the failure within `src`, if known.
        #[label("evaluation failed here")]
        span: Option<SourceSpan>,
        /// Evaluation error reported by CUE.
        message: String,
        /// Suggested remedies for the failure, if any.
        suggestions: Option<Vec<String>>,
    },

    /// Evaluated configuration did not satisfy a schema or semantic check.
    #[error("Validation failed: {message}")]
    #[diagnostic(code(cuenv::validation::failed))]
    Validation {
        /// Source text the diagnostic points into, if available.
        #[source_code]
        src: Option<String>,
        /// Location of the invalid value within `src`, if known.
        #[label("validation failed")]
        span: Option<SourceSpan>,
        /// Description of the validation failure.
        message: String,
        /// Further errors reported alongside this one as related diagnostics.
        #[related]
        related: Vec<Error>,
    },
}

/// I/O and text-encoding errors with operation context.
#[derive(ThisError, Debug, Diagnostic)]
// Intentionally exhaustive: `cuenv`'s `From<cuenv_core::Error> for CliError` matches every variant so a new one forces an explicit exit-code decision.
pub enum IoError {
    /// A filesystem or other I/O operation failed.
    #[error("I/O {operation} failed{}", path.as_ref().map_or(String::new(), |p| format!(": {}", p.display())))]
    #[diagnostic(
        code(cuenv::io::error),
        help("Check file permissions and ensure the path exists")
    )]
    Io {
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
        /// Path the operation acted on, if known.
        path: Option<Box<Path>>,
        /// Description of the operation that failed, such as "read".
        operation: String,
    },

    /// Bytes that had to be UTF-8 text were not valid UTF-8.
    #[error("Text encoding error")]
    #[diagnostic(
        code(cuenv::encoding::utf8),
        help("The file contains invalid UTF-8. Ensure your files use UTF-8 encoding.")
    )]
    Utf8 {
        /// Underlying decoding error.
        #[source]
        source: std::str::Utf8Error,
        /// File the invalid text came from, if known.
        file: Option<Box<Path>>,
    },
}

/// Main error type for cuenv operations: a thin transparent composition of
/// the per-domain errors. Display, diagnostic codes, help, and source spans
/// are forwarded from the wrapped domain error.
#[derive(ThisError, Debug, Diagnostic)]
// Intentionally exhaustive: `cuenv`'s `From<cuenv_core::Error> for CliError` matches every variant so a new one forces an explicit exit-code decision.
pub enum Error {
    /// Invalid or inconsistent configuration.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Configuration(#[from] ConfigError),

    /// CUE evaluation, FFI bridge, or validation failure.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Eval(#[from] EvalError),

    /// I/O or text-encoding failure.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Io(#[from] IoError),

    /// Task graph construction or task execution failure.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Task(#[from] TaskError),

    /// Tool resolution or platform support failure.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Tool(#[from] ToolError),

    /// Secret resolution failure for an environment value.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Secret(#[from] SecretResolutionError),

    /// A command cuenv ran on the user's behalf exited unsuccessfully, and
    /// cuenv's exit status is that command's. The command already reported
    /// its own failure.
    #[error("command exited with code {code}")]
    #[diagnostic(code(cuenv::child_exit))]
    ChildExit {
        /// The exit status to propagate.
        code: i32,
    },
}

impl Error {
    /// Creates a [`ConfigError`] with a message and no source context.
    #[must_use]
    pub fn configuration(msg: impl Into<String>) -> Self {
        ConfigError::new(msg).into()
    }

    /// Creates a [`ConfigError`] that highlights `span` within the
    /// configuration source `src`.
    #[must_use]
    pub fn configuration_with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        ConfigError::with_source(msg, src, span).into()
    }

    /// Creates an [`EvalError::Ffi`] for a failed call to the bridge function
    /// `function`.
    #[must_use]
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        EvalError::Ffi {
            function,
            message: message.into(),
            help: None,
        }
        .into()
    }

    /// Creates an [`EvalError::Ffi`] for a failed call to the bridge function
    /// `function`, with a suggested remedy.
    #[must_use]
    pub fn ffi_with_help(
        function: &'static str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        EvalError::Ffi {
            function,
            message: message.into(),
            help: Some(help.into()),
        }
        .into()
    }

    /// Creates an [`EvalError::CueParse`] for a failed evaluation of `path`
    /// without source context.
    #[must_use]
    pub fn cue_parse(path: &Path, message: impl Into<String>) -> Self {
        EvalError::CueParse {
            path: path.into(),
            src: None,
            span: None,
            message: message.into(),
            suggestions: None,
        }
        .into()
    }

    /// Creates an [`EvalError::CueParse`] for a failed evaluation of `path`,
    /// carrying the CUE source, span, and suggestions from `source`.
    #[must_use]
    pub fn cue_parse_with_source(
        path: &Path,
        message: impl Into<String>,
        source: CueParseSource,
    ) -> Self {
        EvalError::CueParse {
            path: path.into(),
            src: Some(source.text),
            span: source.span,
            message: message.into(),
            suggestions: source.suggestions,
        }
        .into()
    }

    /// Creates an [`EvalError::Validation`] with a message and no source
    /// context.
    #[must_use]
    pub fn validation(msg: impl Into<String>) -> Self {
        EvalError::Validation {
            src: None,
            span: None,
            message: msg.into(),
            related: Vec::new(),
        }
        .into()
    }

    /// Creates an [`EvalError::Validation`] that highlights `span` within the
    /// source `src`.
    #[must_use]
    pub fn validation_with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        EvalError::Validation {
            src: Some(src.into()),
            span,
            message: msg.into(),
            related: Vec::new(),
        }
        .into()
    }

    /// Creates a [`TaskError::Execution`] with a message and no help text.
    #[must_use]
    pub fn execution(msg: impl Into<String>) -> Self {
        TaskError::execution(msg).into()
    }

    /// Creates a [`TaskError::Execution`] with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn execution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::execution_with_help(msg, help).into()
    }

    /// Creates a [`ToolError::Resolution`] with a message and no help text.
    #[must_use]
    pub fn tool_resolution(msg: impl Into<String>) -> Self {
        ToolError::resolution(msg).into()
    }

    /// Creates a [`ToolError::Resolution`] with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn tool_resolution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        ToolError::resolution_with_help(msg, help).into()
    }

    /// Creates a [`ToolError::Platform`] for a platform the tool does not
    /// support.
    #[must_use]
    pub fn platform(msg: impl Into<String>) -> Self {
        ToolError::platform(msg).into()
    }

    /// Creates a [`TaskError::TaskFailed`] for `task_name`, which exited with
    /// `exit_code` and produced `output`.
    #[must_use]
    pub fn task_failed(
        task_name: impl Into<String>,
        exit_code: i32,
        output: crate::tasks::TaskFailureOutput<'_>,
    ) -> Self {
        TaskError::task_failed(task_name, exit_code, output).into()
    }

    /// Creates a [`TaskError::TaskFailed`] for `task_name`, which exited with
    /// `exit_code`, carrying its output and help text from `details`.
    #[must_use]
    pub fn task_failed_with_help(
        task_name: impl Into<String>,
        exit_code: i32,
        details: crate::tasks::TaskFailureWithHelp<'_>,
    ) -> Self {
        TaskError::task_failed_with_help(task_name, exit_code, details).into()
    }

    /// Creates a [`TaskError::TaskGraph`] with a message and no help text.
    #[must_use]
    pub fn task_graph(message: impl Into<String>) -> Self {
        TaskError::graph(message).into()
    }

    /// Creates a [`TaskError::TaskGraph`] with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn task_graph_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::graph_with_help(message, help).into()
    }

    /// Creates a [`SecretResolutionError`] with a message and no help text.
    #[must_use]
    pub fn secret_resolution(message: impl Into<String>) -> Self {
        SecretResolutionError::new(message).into()
    }

    /// Creates a [`SecretResolutionError`] with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn secret_resolution_with_help(
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        SecretResolutionError::with_help(message, help).into()
    }

    /// Creates an [`IoError::Io`] for the failed `operation` without a path.
    #[must_use]
    pub fn io(operation: impl Into<String>, source: std::io::Error) -> Self {
        IoError::Io {
            source,
            path: None,
            operation: operation.into(),
        }
        .into()
    }

    /// Creates an [`IoError::Io`] for the failed `operation` on `path`.
    #[must_use]
    pub fn io_with_path(
        operation: impl Into<String>,
        path: impl Into<Box<Path>>,
        source: std::io::Error,
    ) -> Self {
        IoError::Io {
            source,
            path: Some(path.into()),
            operation: operation.into(),
        }
        .into()
    }

    /// Creates a [`TaskError::Timeout`] for an operation that did not finish
    /// within `seconds` seconds.
    #[must_use]
    pub fn timeout(seconds: u64) -> Self {
        TaskError::Timeout { seconds }.into()
    }
}

// Implement conversions for common error types
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::io("unknown (unmapped error conversion)", source)
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(source: std::str::Utf8Error) -> Self {
        IoError::Utf8 { source, file: None }.into()
    }
}

impl From<cuenv_manifest::lockfile::LockfileError> for Error {
    fn from(source: cuenv_manifest::lockfile::LockfileError) -> Self {
        Self::configuration(source.to_string())
    }
}

impl From<cuenv_hooks::Error> for Error {
    fn from(source: cuenv_hooks::Error) -> Self {
        TaskError::execution(source.to_string()).into()
    }
}

impl From<cuenv_task_graph::Error> for Error {
    fn from(err: cuenv_task_graph::Error) -> Self {
        TaskError::from(err).into()
    }
}

/// Result type alias for cuenv operations
pub type Result<T> = std::result::Result<T, Error>;
