//! Secret-resolution error type (RFC-0006 phase 2e).
//!
//! Owns the secret-resolution variant of the former monolithic
//! `cuenv_core::Error`. Distinct from `cuenv_secrets::SecretError` (the
//! resolver-level error): this is the core-level wrapper surfaced to the CLI.

use miette::Diagnostic;
use thiserror::Error as ThisError;

/// Secret resolution failed for an environment value.
#[derive(ThisError, Debug, Diagnostic)]
#[error("Secret resolution failed: {message}")]
#[diagnostic(code(cuenv::secret::resolution))]
pub struct SecretResolutionError {
    /// Description of why the secret could not be resolved.
    pub message: String,
    /// Suggested remedy shown with the diagnostic, if any.
    #[help]
    pub help: Option<String>,
}

impl SecretResolutionError {
    /// Creates a secret resolution error with a message and no help text.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            help: None,
        }
    }

    /// Creates a secret resolution error with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn with_help(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            help: Some(help.into()),
        }
    }
}
