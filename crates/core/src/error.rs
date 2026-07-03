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
    #[source_code]
    pub src: String,
    #[label("invalid configuration")]
    pub span: Option<SourceSpan>,
    pub message: String,
}

impl ConfigError {
    #[must_use]
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            src: String::new(),
            span: None,
            message: msg.into(),
        }
    }

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
pub enum EvalError {
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
}

/// I/O and text-encoding errors with operation context.
#[derive(ThisError, Debug, Diagnostic)]
pub enum IoError {
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
}

/// Main error type for cuenv operations: a thin transparent composition of
/// the per-domain errors. Display, diagnostic codes, help, and source spans
/// are forwarded from the wrapped domain error.
#[derive(ThisError, Debug, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Configuration(#[from] ConfigError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Eval(#[from] EvalError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Io(#[from] IoError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Task(#[from] TaskError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Tool(#[from] ToolError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Secret(#[from] SecretResolutionError),
}

impl Error {
    #[must_use]
    pub fn configuration(msg: impl Into<String>) -> Self {
        ConfigError::new(msg).into()
    }

    #[must_use]
    pub fn configuration_with_source(
        msg: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> Self {
        ConfigError::with_source(msg, src, span).into()
    }

    #[must_use]
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        EvalError::Ffi {
            function,
            message: message.into(),
            help: None,
        }
        .into()
    }

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

    #[must_use]
    pub fn cue_parse_with_source(
        path: &Path,
        message: impl Into<String>,
        src: impl Into<String>,
        span: Option<SourceSpan>,
        suggestions: Option<Vec<String>>,
    ) -> Self {
        EvalError::CueParse {
            path: path.into(),
            src: Some(src.into()),
            span,
            message: message.into(),
            suggestions,
        }
        .into()
    }

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

    #[must_use]
    pub fn execution(msg: impl Into<String>) -> Self {
        TaskError::execution(msg).into()
    }

    #[must_use]
    pub fn execution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::execution_with_help(msg, help).into()
    }

    #[must_use]
    pub fn tool_resolution(msg: impl Into<String>) -> Self {
        ToolError::resolution(msg).into()
    }

    #[must_use]
    pub fn tool_resolution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        ToolError::resolution_with_help(msg, help).into()
    }

    #[must_use]
    pub fn platform(msg: impl Into<String>) -> Self {
        ToolError::platform(msg).into()
    }

    #[must_use]
    pub fn task_failed(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        TaskError::task_failed(task_name, exit_code, stdout, stderr).into()
    }

    #[must_use]
    pub fn task_failed_with_help(
        task_name: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        TaskError::task_failed_with_help(task_name, exit_code, stdout, stderr, help).into()
    }

    #[must_use]
    pub fn task_graph(message: impl Into<String>) -> Self {
        TaskError::graph(message).into()
    }

    #[must_use]
    pub fn task_graph_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        TaskError::graph_with_help(message, help).into()
    }

    #[must_use]
    pub fn secret_resolution(message: impl Into<String>) -> Self {
        SecretResolutionError::new(message).into()
    }

    #[must_use]
    pub fn secret_resolution_with_help(
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        SecretResolutionError::with_help(message, help).into()
    }

    #[must_use]
    pub fn io(operation: impl Into<String>, source: std::io::Error) -> Self {
        IoError::Io {
            source,
            path: None,
            operation: operation.into(),
        }
        .into()
    }

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

    #[must_use]
    pub fn timeout(seconds: u64) -> Self {
        TaskError::Timeout { seconds }.into()
    }
}

// Implement conversions for common error types
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::io("unknown (unmapped error conversion)", source)
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(source: std::str::Utf8Error) -> Self {
        IoError::Utf8 { source, file: None }.into()
    }
}

impl From<cuenv_manifest::lockfile::LockfileError> for Error {
    fn from(source: cuenv_manifest::lockfile::LockfileError) -> Self {
        Error::configuration(source.to_string())
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
