//! Error types for the Dagger 1.0 backend and emitter.

use thiserror::Error;

/// Result type for `cuenv-dagger` operations.
pub type Result<T> = std::result::Result<T, DaggerError>;

/// Errors returned by the Dagger Engine session, GraphQL client, or runner.
#[derive(Debug, Error)]
pub enum DaggerError {
    /// Failed to spawn or communicate with the `dagger session` process.
    #[error("dagger session error: {0}")]
    Session(String),

    /// HTTP transport error talking to the Dagger Engine GraphQL endpoint.
    #[error("dagger HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// GraphQL query returned one or more errors from the Dagger Engine.
    #[error("dagger GraphQL error: {0}")]
    GraphQl(String),

    /// Unexpected or malformed response payload from the Dagger Engine.
    #[error("invalid dagger response: {0}")]
    InvalidResponse(String),

    /// Invalid task or runtime configuration for Dagger execution.
    #[error("invalid dagger configuration: {0}")]
    Configuration(String),

    /// I/O error interacting with local files or processes.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<DaggerError> for cuenv_core::Error {
    fn from(err: DaggerError) -> Self {
        match err {
            DaggerError::Configuration(msg) => Self::configuration(msg),
            other => Self::execution(other.to_string()),
        }
    }
}
