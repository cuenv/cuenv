//! Tool-domain error type (RFC-0006 phase 2e).
//!
//! Owns the tool-resolution and platform variants of the former monolithic
//! `cuenv_core::Error`. Constructed mainly by the tool provider crates via
//! the `Error::tool_resolution`/`Error::platform` helpers.

use miette::Diagnostic;
use thiserror::Error as ThisError;

/// Errors raised while resolving tools or checking platform support.
#[derive(ThisError, Debug, Diagnostic)]
pub enum ToolError {
    #[error("Tool resolution failed: {message}")]
    #[diagnostic(code(cuenv::tool::resolution))]
    Resolution {
        message: String,
        #[help]
        help: Option<String>,
    },

    #[error("Platform error: {message}")]
    #[diagnostic(
        code(cuenv::platform::error),
        help("This platform may not be supported by the tool provider")
    )]
    Platform { message: String },
}

impl ToolError {
    #[must_use]
    pub fn resolution(msg: impl Into<String>) -> Self {
        ToolError::Resolution {
            message: msg.into(),
            help: None,
        }
    }

    #[must_use]
    pub fn resolution_with_help(msg: impl Into<String>, help: impl Into<String>) -> Self {
        ToolError::Resolution {
            message: msg.into(),
            help: Some(help.into()),
        }
    }

    #[must_use]
    pub fn platform(msg: impl Into<String>) -> Self {
        ToolError::Platform {
            message: msg.into(),
        }
    }
}
