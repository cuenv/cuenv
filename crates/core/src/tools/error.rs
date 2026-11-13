//! Tool-domain error type (RFC-0006 phase 2e).
//!
//! Owns the tool-resolution and platform variants of the former monolithic
//! `cuenv_core::Error`. Constructed mainly by the tool provider crates via
//! the `Error::tool_resolution`/`Error::platform` helpers.

use miette::Diagnostic;
use thiserror::Error as ThisError;

/// Errors raised while resolving tools or checking platform support.
#[derive(ThisError, Debug, Diagnostic)]
// Intentionally exhaustive: `cuenv`'s `From<cuenv_core::Error> for CliError` matches every variant so a new one forces an explicit exit-code decision.
pub enum ToolError {
    /// A tool could not be resolved to a concrete artifact.
    #[error("Tool resolution failed: {message}")]
    #[diagnostic(code(cuenv::tool::resolution))]
    Resolution {
        /// Description of why resolution failed.
        message: String,
        /// Suggested remedy shown with the diagnostic, if any.
        #[help]
        help: Option<String>,
    },

    /// The current platform is unknown or unsupported for a tool.
    #[error("Platform error: {message}")]
    #[diagnostic(
        code(cuenv::platform::error),
        help("This platform may not be supported by the tool provider")
    )]
    Platform {
        /// Description of the platform problem.
        message: String,
    },
}

impl ToolError {
    /// Creates a [`Self::Resolution`] error with a message and no help text.
    #[must_use]
    pub fn resolution(msg: impl Into<String>) -> Self {
        Self::Resolution {
            message: msg.into(),
            help: None,
        }
    }

    /// Creates a [`Self::Resolution`] error with a message and a suggested
    /// remedy.
    #[must_use]
    pub fn resolution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Resolution {
            message: msg.into(),
            help: Some(help.into()),
        }
    }

    /// Creates a [`Self::Platform`] error with a message.
    #[must_use]
    pub fn platform(msg: impl Into<String>) -> Self {
        Self::Platform {
            message: msg.into(),
        }
    }
}
