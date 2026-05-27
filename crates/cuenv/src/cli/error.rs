//! CLI error mapping, rendering, and exit codes.

use super::output::{ErrorEnvelope, OutputFormat};
use miette::{Diagnostic, Report};
use std::io::{self, Write};
use thiserror::Error;

/// Exit codes for the CLI application
pub const EXIT_OK: i32 = 0;
/// CLI or configuration error exit code
pub const EXIT_CLI: i32 = 2;
/// CUE evaluation or FFI error exit code
pub const EXIT_EVAL: i32 = 3;

/// CLI-specific error types with proper exit code mapping
#[derive(Error, Debug, Clone, Diagnostic)]
pub enum CliError {
    /// CLI or configuration error (exit code 2)
    #[error("CLI/configuration error: {message}")]
    #[diagnostic(code(cuenv::cli::config))]
    Config {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
    /// CUE evaluation or FFI error (exit code 3)
    #[error("Evaluation/FFI error: {message}")]
    #[diagnostic(code(cuenv::cli::eval))]
    Eval {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
    /// Other unexpected error (exit code 3)
    #[error("Unexpected error: {message}")]
    #[diagnostic(code(cuenv::cli::other))]
    Other {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
    },
}

impl CliError {
    /// Create a new configuration error
    #[must_use]
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new configuration error with help text
    #[must_use]
    pub fn config_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    /// Create a new evaluation error
    #[must_use]
    pub fn eval(message: impl Into<String>) -> Self {
        Self::Eval {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new evaluation error with help text
    #[must_use]
    pub fn eval_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Eval {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    /// Create a new other error
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other {
            message: message.into(),
            help: None,
        }
    }

    /// Create a new other error with help text
    #[must_use]
    pub fn other_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Other {
            message: message.into(),
            help: Some(help.into()),
        }
    }

    /// Add help text to an existing error, returning a new error with the help text set.
    #[must_use]
    pub fn with_help(self, help_text: impl Into<String>) -> Self {
        let help = Some(help_text.into());
        match self {
            Self::Config { message, .. } => Self::Config { message, help },
            Self::Eval { message, .. } => Self::Eval { message, help },
            Self::Other { message, .. } => Self::Other { message, help },
        }
    }
}

/// Convert `cuenv_core::Error` to appropriate `CliError` variant.
///
/// Maps error types to their appropriate CLI categories:
/// - Configuration errors (task not found, invalid config) -> Config (exit code 2)
/// - FFI/CUE evaluation errors -> Eval (exit code 3)
/// - I/O and other errors -> Other (exit code 3)
impl From<cuenv_core::Error> for CliError {
    fn from(err: cuenv_core::Error) -> Self {
        match err {
            cuenv_core::Error::Configuration { message, .. } => Self::config(message),
            cuenv_core::Error::Ffi { .. }
            | cuenv_core::Error::CueParse { .. }
            | cuenv_core::Error::Validation { .. } => Self::eval(err.to_string()),
            cuenv_core::Error::Execution { message, .. } => {
                Self::eval_with_help(message, "Check the task output above for details")
            }
            cuenv_core::Error::ToolResolution { message, help } => {
                if let Some(h) = help {
                    Self::eval_with_help(message, h)
                } else {
                    Self::eval(message)
                }
            }
            cuenv_core::Error::Platform { message } => Self::eval(message),
            cuenv_core::Error::TaskFailed {
                task_name,
                exit_code,
                stderr,
                help,
                ..
            } => {
                let stderr_snippet = if stderr.trim().is_empty() {
                    String::new()
                } else {
                    let lines: Vec<&str> = stderr.lines().collect();
                    let start = lines.len().saturating_sub(10);
                    format!("\n\nstderr:\n{}", lines[start..].join("\n"))
                };
                let message = format!(
                    "Task '{}' failed with exit code {}{}",
                    task_name, exit_code, stderr_snippet
                );
                if let Some(h) = help {
                    Self::eval_with_help(message, h)
                } else {
                    Self::eval_with_help(message, "Check the task output above for details")
                }
            }
            cuenv_core::Error::TaskGraph { message, help } => {
                if let Some(h) = help {
                    Self::config_with_help(message, h)
                } else {
                    Self::config(message)
                }
            }
            cuenv_core::Error::SecretResolution { message, help } => {
                if let Some(h) = help {
                    Self::eval_with_help(message, h)
                } else {
                    Self::eval_with_help(
                        message,
                        "Check your secret provider configuration (1Password, AWS, Vault, etc.)",
                    )
                }
            }
            cuenv_core::Error::Io {
                source,
                path,
                operation,
            } => {
                let path_str = path
                    .as_ref()
                    .map_or(String::new(), |p| format!(" on {}", p.display()));
                Self::other_with_help(
                    format!("I/O {operation} failed{path_str}: {source}"),
                    "Check file permissions and ensure the path exists",
                )
            }
            cuenv_core::Error::Utf8 { .. } | cuenv_core::Error::Timeout { .. } => {
                Self::other(err.to_string())
            }
        }
    }
}

/// Map CLI error to appropriate exit code
#[must_use]
pub const fn exit_code_for(err: &CliError) -> i32 {
    match err {
        CliError::Config { .. } => EXIT_CLI,
        CliError::Eval { .. } | CliError::Other { .. } => EXIT_EVAL,
    }
}

/// Render error appropriately based on output format
pub fn render_error(err: &CliError, format: OutputFormat) {
    if format.is_json() {
        let error_envelope = ErrorEnvelope::new(serde_json::json!({
            "code": match err {
                CliError::Config { .. } => "config",
                CliError::Eval { .. } => "eval",
                CliError::Other { .. } => "other",
            },
            "message": err.to_string()
        }));

        match serde_json::to_string(&error_envelope) {
            Ok(json) => cuenv_events::println_redacted(&json),
            Err(_) => {
                cuenv_events::eprintln_redacted("Error serializing error response");
            }
        }
    } else {
        let report = Report::new(err.clone());
        cuenv_events::eprintln_redacted(&format!("{report:?}"));
        let _ = io::stderr().flush();
    }
}
