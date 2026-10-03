//! CLI error mapping, rendering, and exit codes.

use super::output::{ErrorEnvelope, OutputFormat};
use miette::{Diagnostic, Report};
use std::collections::BTreeMap;
use std::io::{self, Write};
use thiserror::Error;

/// Exit codes for the CLI application
pub const EXIT_OK: i32 = 0;
/// An infrastructure change was not confirmed at the prompt (declined, or
/// no answer could be read); nothing was applied
pub const EXIT_CANCELLED: i32 = 1;
/// CLI or configuration error exit code
pub const EXIT_CLI: i32 = 2;
/// CUE evaluation or FFI error exit code
pub const EXIT_EVAL: i32 = 3;
/// Infrastructure run collided with concurrent activity (the state is locked
/// by another run, or `unlock` named a lock that is not the current one);
/// retrying later can succeed
pub const EXIT_LOCKED: i32 = 4;
/// Infrastructure provider, state store, or apply failure
pub const EXIT_INFRASTRUCTURE: i32 = 5;
/// Interrupted: 128 + SIGINT, the shell convention for an interrupted
/// command. Every command stopped by Ctrl-C exits with it, and
/// `cuenv infrastructure` also when stopped by SIGTERM, SIGHUP or SIGQUIT
pub const EXIT_INTERRUPTED: i32 = 130;

/// What kind of infrastructure failure occurred; decides the exit code and
/// the JSON error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfrastructureFailureKind {
    /// Another run holds the project's lock, or a lock other than the one
    /// named. Exit code 4, JSON code `infrastructure_locked`.
    Locked,
    /// The plan was not confirmed at the prompt (declined, or no answer
    /// could be read); nothing was applied. Exit code 1, JSON code
    /// `infrastructure_cancelled`.
    Cancelled,
    /// The run was stopped by a signal: whatever was applied is recorded.
    /// The error's [`LockStatus`] says whether the lock was released. Exit
    /// code 130, JSON code `infrastructure_interrupted`.
    Interrupted,
    /// Any other provider, state store or apply failure. Exit code 5, JSON
    /// code `infrastructure`.
    Failed,
}

/// The state lock an infrastructure error concerns and whether it is
/// released now; `lockIdentifier` and `lockReleased` in the JSON error
/// envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockStatus {
    /// Identifier of the lock, as `cuenv infrastructure unlock` takes it.
    pub identifier: String,
    /// Whether the lock is known to be released: `false` when it is still
    /// held or when that is not known.
    pub released: bool,
}

/// Replace every registered secret in `text`.
///
/// Errors are redacted when they are built, from the raw strings: the
/// terminal renderer wraps long lines, which can split a secret across two
/// lines where no later search would find it, and JSON escapes quotes and
/// backslashes inside a secret.
fn redacted(text: impl Into<String>) -> String {
    cuenv_events::redact(&text.into())
}

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
    /// Infrastructure failure (exit code 1 when cancelled, 4 for concurrent
    /// activity, 130 when interrupted, otherwise 5)
    #[error("Infrastructure error: {message}")]
    #[diagnostic(code(cuenv::cli::infrastructure))]
    Infrastructure {
        /// The error message
        message: String,
        /// Optional help text
        #[help]
        help: Option<String>,
        /// What kind of failure it is
        kind: InfrastructureFailureKind,
        /// The state lock the failure concerns, when there is one
        lock: Option<LockStatus>,
        /// Addresses of replacements whose delete finished and whose create
        /// did not: the objects are gone until the next apply recreates
        /// them. Empty when there are none.
        deleted_not_recreated: Vec<String>,
        /// Further JSON fields of the error envelope, by name: facts a
        /// script needs beyond the code and message (the locks that block a
        /// migration, the result of an operation that finished but could not
        /// release its lock).
        /// Boxed, so the error stays small enough to return everywhere.
        details: Option<Box<BTreeMap<String, serde_json::Value>>>,
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
            message: redacted(message),
            help: None,
        }
    }

    /// Create a new configuration error with help text
    #[must_use]
    pub fn config_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Config {
            message: redacted(message),
            help: Some(redacted(help)),
        }
    }

    /// Create a new evaluation error
    #[must_use]
    pub fn eval(message: impl Into<String>) -> Self {
        Self::Eval {
            message: redacted(message),
            help: None,
        }
    }

    /// Create a new evaluation error with help text
    #[must_use]
    pub fn eval_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Eval {
            message: redacted(message),
            help: Some(redacted(help)),
        }
    }

    /// Create a new infrastructure error
    #[must_use]
    pub fn infrastructure(
        message: impl Into<String>,
        help: Option<String>,
        kind: InfrastructureFailureKind,
    ) -> Self {
        Self::Infrastructure {
            message: redacted(message),
            help: help.map(redacted),
            kind,
            lock: None,
            deleted_not_recreated: Vec::new(),
            details: None,
        }
    }

    /// Add a field to the JSON error envelope of an infrastructure error;
    /// any other error is returned unchanged. The value is redacted with the
    /// rest of the envelope.
    #[must_use]
    pub fn with_detail(self, name: &str, value: serde_json::Value) -> Self {
        match self {
            Self::Infrastructure {
                message,
                help,
                kind,
                lock,
                deleted_not_recreated,
                details,
            } => {
                let mut details = details.map(|boxed| *boxed).unwrap_or_default();
                details.insert(name.to_string(), value);
                Self::Infrastructure {
                    message,
                    help,
                    kind,
                    lock,
                    deleted_not_recreated,
                    details: Some(Box::new(details)),
                }
            }
            other => other,
        }
    }

    /// Attach the state lock an infrastructure error concerns; any other
    /// error is returned unchanged.
    #[must_use]
    pub fn with_lock(self, status: LockStatus) -> Self {
        match self {
            Self::Infrastructure {
                message,
                help,
                kind,
                deleted_not_recreated,
                details,
                ..
            } => Self::Infrastructure {
                message,
                help,
                kind,
                lock: Some(status),
                deleted_not_recreated,
                details,
            },
            other => other,
        }
    }

    /// Say that these replacements were deleted and not recreated, in the
    /// help text and in the JSON envelope (`deletedNotRecreated`); any other
    /// error, or an empty list, is returned unchanged.
    #[must_use]
    pub fn with_deleted_not_recreated(self, addresses: Vec<String>) -> Self {
        match self {
            Self::Infrastructure {
                message,
                help,
                kind,
                lock,
                details,
                ..
            } if !addresses.is_empty() => {
                let listed = addresses.join(", ");
                let notice = format!(
                    "Deleted but NOT recreated: {listed}. They no longer exist; run apply again \
                     to recreate them."
                );
                Self::Infrastructure {
                    message,
                    help: Some(redacted(
                        help.map_or_else(|| notice.clone(), |help| format!("{help} {notice}")),
                    )),
                    kind,
                    lock,
                    deleted_not_recreated: addresses,
                    details,
                }
            }
            other => other,
        }
    }

    /// The message, without the category prefix of its display.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Config { message, .. }
            | Self::Eval { message, .. }
            | Self::Infrastructure { message, .. }
            | Self::Other { message, .. } => message,
        }
    }

    /// The help text, when there is one.
    #[must_use]
    pub fn help(&self) -> Option<&str> {
        match self {
            Self::Config { help, .. }
            | Self::Eval { help, .. }
            | Self::Infrastructure { help, .. }
            | Self::Other { help, .. } => help.as_deref(),
        }
    }

    /// Create a new other error
    #[must_use]
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other {
            message: redacted(message),
            help: None,
        }
    }

    /// Create a new other error with help text
    #[must_use]
    pub fn other_with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Other {
            message: redacted(message),
            help: Some(redacted(help)),
        }
    }

    /// The error with every registered secret replaced in its message and
    /// help, including secrets registered after it was built. Constructors
    /// redact already; this is the last check before an error is shown.
    #[must_use]
    pub fn with_secrets_redacted(self) -> Self {
        match self {
            Self::Config { message, help } => Self::Config {
                message: redacted(message),
                help: help.map(redacted),
            },
            Self::Eval { message, help } => Self::Eval {
                message: redacted(message),
                help: help.map(redacted),
            },
            Self::Other { message, help } => Self::Other {
                message: redacted(message),
                help: help.map(redacted),
            },
            Self::Infrastructure {
                message,
                help,
                kind,
                lock,
                deleted_not_recreated,
                details,
            } => Self::Infrastructure {
                message: redacted(message),
                help: help.map(redacted),
                kind,
                lock,
                deleted_not_recreated,
                details,
            },
        }
    }

    /// Add help text to an existing error, returning a new error with the help text set.
    #[must_use]
    pub fn with_help(self, help_text: impl Into<String>) -> Self {
        let help = Some(redacted(help_text));
        match self {
            Self::Config { message, .. } => Self::Config { message, help },
            Self::Eval { message, .. } => Self::Eval { message, help },
            Self::Other { message, .. } => Self::Other { message, help },
            Self::Infrastructure {
                message,
                kind,
                lock,
                deleted_not_recreated,
                details,
                ..
            } => Self::Infrastructure {
                message,
                help,
                kind,
                lock,
                deleted_not_recreated,
                details,
            },
        }
    }
}

/// Convert `cuenv_core::Error` to appropriate `CliError` variant.
///
/// Maps error types to their appropriate CLI categories:
/// - Configuration and task-graph errors (task not found, invalid config) -> Config (exit code 2)
/// - FFI/CUE evaluation, task, tool, and secret errors -> Eval (exit code 3)
/// - I/O and other errors -> Other (exit code 3)
///
/// The match is exhaustive at both levels (no wildcards) so new domain
/// variants force an explicit exit-code decision here.
impl From<cuenv_core::Error> for CliError {
    fn from(err: cuenv_core::Error) -> Self {
        match err {
            cuenv_core::Error::Configuration(cuenv_core::ConfigError { message, .. }) => {
                Self::config(message)
            }
            cuenv_core::Error::Eval(eval_err) => Self::eval(eval_err.to_string()),
            cuenv_core::Error::Task(task_err) => match task_err {
                cuenv_core::TaskError::Execution { message, .. } => {
                    Self::eval_with_help(message, "Check the task output above for details")
                }
                cuenv_core::TaskError::TaskFailed {
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
                cuenv_core::TaskError::TaskGraph { message, help } => {
                    if let Some(h) = help {
                        Self::config_with_help(message, h)
                    } else {
                        Self::config(message)
                    }
                }
                timeout @ cuenv_core::TaskError::Timeout { .. } => Self::other(timeout.to_string()),
            },
            cuenv_core::Error::Tool(tool_err) => match tool_err {
                cuenv_core::ToolError::Resolution { message, help } => {
                    if let Some(h) = help {
                        Self::eval_with_help(message, h)
                    } else {
                        Self::eval(message)
                    }
                }
                cuenv_core::ToolError::Platform { message } => Self::eval(message),
            },
            cuenv_core::Error::Secret(cuenv_core::SecretResolutionError { message, help }) => {
                if let Some(h) = help {
                    Self::eval_with_help(message, h)
                } else {
                    Self::eval_with_help(
                        message,
                        "Check your secret provider configuration (1Password, AWS, Vault, etc.)",
                    )
                }
            }
            cuenv_core::Error::Io(io_err) => match io_err {
                cuenv_core::IoError::Io {
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
                utf8 @ cuenv_core::IoError::Utf8 { .. } => Self::other(utf8.to_string()),
            },
        }
    }
}

/// Map CLI error to appropriate exit code
#[must_use]
pub const fn exit_code_for(err: &CliError) -> i32 {
    match err {
        CliError::Config { .. } => EXIT_CLI,
        CliError::Eval { .. } | CliError::Other { .. } => EXIT_EVAL,
        CliError::Infrastructure {
            kind: InfrastructureFailureKind::Cancelled,
            ..
        } => EXIT_CANCELLED,
        CliError::Infrastructure {
            kind: InfrastructureFailureKind::Locked,
            ..
        } => EXIT_LOCKED,
        CliError::Infrastructure {
            kind: InfrastructureFailureKind::Interrupted,
            ..
        } => EXIT_INTERRUPTED,
        CliError::Infrastructure {
            kind: InfrastructureFailureKind::Failed,
            ..
        } => EXIT_INFRASTRUCTURE,
    }
}

/// The `code` of an error in the JSON error envelope.
#[must_use]
pub const fn error_code_for(err: &CliError) -> &'static str {
    match err {
        CliError::Config { .. } => "config",
        CliError::Eval { .. } => "eval",
        CliError::Other { .. } => "other",
        CliError::Infrastructure { kind, .. } => match kind {
            InfrastructureFailureKind::Locked => "infrastructure_locked",
            InfrastructureFailureKind::Cancelled => "infrastructure_cancelled",
            InfrastructureFailureKind::Interrupted => "infrastructure_interrupted",
            InfrastructureFailureKind::Failed => "infrastructure",
        },
    }
}

/// The JSON error envelope of an error.
///
/// It holds `code`, `message`, `help` when there is help text,
/// `lockIdentifier` and `lockReleased` when an infrastructure error concerns
/// a state lock, and `deletedNotRecreated` (the addresses) when a failed
/// apply left replacements deleted and not recreated, plus the `details` of an
/// infrastructure error under their own names.
///
/// Every string in it is redacted as a string (never as serialized text,
/// where a secret that JSON escapes would not be found).
#[must_use]
pub fn error_envelope(err: &CliError) -> ErrorEnvelope<serde_json::Value> {
    let mut error = serde_json::Map::new();
    error.insert("code".to_string(), error_code_for(err).into());
    error.insert("message".to_string(), err.to_string().into());
    if let Some(help) = err.help() {
        error.insert("help".to_string(), help.into());
    }
    if let CliError::Infrastructure {
        lock: Some(lock), ..
    } = err
    {
        error.insert("lockIdentifier".to_string(), lock.identifier.clone().into());
        error.insert("lockReleased".to_string(), lock.released.into());
    }
    if let CliError::Infrastructure {
        deleted_not_recreated,
        ..
    } = err
        && !deleted_not_recreated.is_empty()
    {
        error.insert(
            "deletedNotRecreated".to_string(),
            deleted_not_recreated.clone().into(),
        );
    }
    if let CliError::Infrastructure {
        details: Some(details),
        ..
    } = err
    {
        for (name, value) in details.iter() {
            error.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
    let mut error = serde_json::Value::Object(error);
    cuenv_events::redact_json_value(&mut error);
    ErrorEnvelope::new(error)
}

/// The error as the terminal shows it, with every registered secret
/// replaced before the report is laid out and wrapped.
#[must_use]
pub fn error_report_text(err: &CliError) -> String {
    let report = Report::new(err.clone().with_secrets_redacted());
    format!("{report:?}")
}

/// Render error appropriately based on output format
pub fn render_error(err: &CliError, format: OutputFormat) {
    if format.is_json() {
        let error_envelope = error_envelope(err);

        match serde_json::to_string(&error_envelope) {
            Ok(json) => write_standard_output_line(&json),
            Err(_) => {
                cuenv_events::eprintln_redacted("Error serializing error response");
            }
        }
    } else {
        cuenv_events::eprintln_redacted(&error_report_text(err));
        let _ = io::stderr().flush();
    }
}

/// Write one line of already redacted JSON to standard output.
fn write_standard_output_line(line: &str) {
    let mut standard_output = io::stdout().lock();
    if let Err(error) = writeln!(standard_output, "{line}") {
        tracing::debug!(%error, "failed to write the JSON error envelope");
    }
}
