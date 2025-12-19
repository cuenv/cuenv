//! Error types for REAPI client

use miette::Diagnostic;
use thiserror::Error;

/// Result type alias for REAPI operations
pub type Result<T> = std::result::Result<T, RemoteError>;

/// Errors that can occur during remote execution
#[derive(Debug, Error, Diagnostic)]
pub enum RemoteError {
    /// Failed to connect to REAPI server
    #[error("Failed to connect to REAPI server at {endpoint}: {message}")]
    #[diagnostic(
        code(remote::connection_failed),
        help("Check that the endpoint is correct and the server is running")
    )]
    ConnectionFailed { endpoint: String, message: String },

    /// gRPC call failed
    #[error("gRPC call failed: {operation}")]
    #[diagnostic(code(remote::grpc_error))]
    GrpcError {
        operation: String,
        #[source]
        source: tonic::Status,
    },

    /// Content not found in CAS
    #[error("Content not found in CAS: {digest}")]
    #[diagnostic(
        code(remote::content_not_found),
        help("The requested content may have been garbage collected")
    )]
    ContentNotFound { digest: String },

    /// Invalid digest format
    #[error("Invalid digest format: {0}")]
    #[diagnostic(code(remote::invalid_digest))]
    InvalidDigest(String),

    /// Merkle tree construction failed
    #[error("Failed to build Merkle tree")]
    #[diagnostic(code(remote::merkle_error))]
    MerkleError {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Task execution failed remotely
    #[error("Remote execution failed: {message}")]
    #[diagnostic(code(remote::execution_failed))]
    ExecutionFailed { message: String },

    /// Operation timed out
    #[error("Operation timed out after {timeout_secs}s: {operation}")]
    #[diagnostic(
        code(remote::timeout),
        help("Consider increasing the timeout configuration")
    )]
    Timeout {
        operation: String,
        timeout_secs: u64,
    },

    /// Authentication failed
    #[error("Authentication failed")]
    #[diagnostic(
        code(remote::auth_failed),
        help("Check your API key or credentials")
    )]
    AuthenticationFailed {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Configuration error
    #[error("Configuration error: {0}")]
    #[diagnostic(code(remote::config_error))]
    ConfigError(String),

    /// I/O error
    #[error("I/O error: {operation}")]
    #[diagnostic(code(remote::io_error))]
    IoError {
        operation: String,
        #[source]
        source: std::io::Error,
    },

    /// Serialization error
    #[error("Serialization error")]
    #[diagnostic(code(remote::serialization_error))]
    SerializationError {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Retry exhausted
    #[error("Operation failed after {attempts} attempts: {operation}")]
    #[diagnostic(code(remote::retry_exhausted))]
    RetryExhausted {
        operation: String,
        attempts: usize,
    },

    /// Blob upload failed
    #[error("Failed to upload blob {digest}: {message}")]
    #[diagnostic(code(remote::upload_failed))]
    UploadFailed { digest: String, message: String },
}

impl RemoteError {
    /// Create a connection failed error
    pub fn connection_failed(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ConnectionFailed {
            endpoint: endpoint.into(),
            message: message.into(),
        }
    }

    /// Create an upload failed error
    pub fn upload_failed(digest: impl Into<String>, message: impl Into<String>) -> Self {
        Self::UploadFailed {
            digest: digest.into(),
            message: message.into(),
        }
    }

    /// Create a gRPC error
    pub fn grpc_error(operation: impl Into<String>, source: tonic::Status) -> Self {
        Self::GrpcError {
            operation: operation.into(),
            source,
        }
    }

    /// Create a content not found error
    pub fn content_not_found(digest: impl Into<String>) -> Self {
        Self::ContentNotFound {
            digest: digest.into(),
        }
    }

    /// Create an invalid digest error
    pub fn invalid_digest(msg: impl Into<String>) -> Self {
        Self::InvalidDigest(msg.into())
    }

    /// Create a merkle error
    pub fn merkle_error(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::MerkleError {
            source: source.into(),
        }
    }

    /// Create an execution failed error
    pub fn execution_failed(message: impl Into<String>) -> Self {
        Self::ExecutionFailed {
            message: message.into(),
        }
    }

    /// Create a timeout error
    pub fn timeout(operation: impl Into<String>, timeout_secs: u64) -> Self {
        Self::Timeout {
            operation: operation.into(),
            timeout_secs,
        }
    }

    /// Create an authentication failed error
    pub fn auth_failed(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::AuthenticationFailed {
            source: source.into(),
        }
    }

    /// Create a config error
    pub fn config_error(msg: impl Into<String>) -> Self {
        Self::ConfigError(msg.into())
    }

    /// Create an I/O error
    pub fn io_error(operation: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoError {
            operation: operation.into(),
            source,
        }
    }

    /// Create a serialization error
    pub fn serialization_error(
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::SerializationError {
            source: source.into(),
        }
    }

    /// Create a retry exhausted error
    pub fn retry_exhausted(operation: impl Into<String>, attempts: usize) -> Self {
        Self::RetryExhausted {
            operation: operation.into(),
            attempts,
        }
    }
}
