//! Error types for the cuenv-hooks crate

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

/// Main error type for cuenv-hooks operations
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// Configuration error
    #[error("Configuration error: {message}")]
    #[diagnostic(code(cuenv_hooks::config::invalid))]
    Configuration { message: String },

    /// I/O error with path context
    #[error("I/O error during {operation}: {source}")]
    #[diagnostic(code(cuenv_hooks::io::error))]
    Io {
        #[source]
        source: std::io::Error,
        path: Option<Box<std::path::Path>>,
        operation: String,
    },

    /// Timeout error
    #[error("Operation timed out after {seconds} seconds")]
    #[diagnostic(code(cuenv_hooks::timeout))]
    Timeout { seconds: u64 },

    /// State not found error
    #[error("Execution state not found for instance: {instance_id}")]
    #[diagnostic(code(cuenv_hooks::state::not_found))]
    StateNotFound { instance_id: String },

    /// Serialization/deserialization error
    #[error("Serialization error: {message}")]
    #[diagnostic(code(cuenv_hooks::serialization))]
    Serialization { message: String },

    /// Process execution error
    #[error("Process execution failed: {message}")]
    #[diagnostic(code(cuenv_hooks::process))]
    Process { message: String },
}

impl Error {
    /// Create a configuration error with a message
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration {
            message: message.into(),
        }
    }

    /// Create an I/O error with context
    pub fn io(source: std::io::Error, path: Option<PathBuf>, operation: impl Into<String>) -> Self {
        Self::Io {
            source,
            path: path.map(|p| p.into_boxed_path()),
            operation: operation.into(),
        }
    }

    /// Create a state not found error
    pub fn state_not_found(instance_id: impl Into<String>) -> Self {
        Self::StateNotFound {
            instance_id: instance_id.into(),
        }
    }

    /// Create a serialization error
    pub fn serialization(message: impl Into<String>) -> Self {
        Self::Serialization {
            message: message.into(),
        }
    }

    /// Create a process execution error
    pub fn process(message: impl Into<String>) -> Self {
        Self::Process {
            message: message.into(),
        }
    }
}

/// Result type for cuenv-hooks operations
pub type Result<T> = std::result::Result<T, Error>;
