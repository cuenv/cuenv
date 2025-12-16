//! Error types for the IaC system.

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

/// Result type alias using the IaC error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in the IaC system.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// No configuration has been loaded.
    #[error("No configuration loaded. Call load_config() first.")]
    #[diagnostic(code(cuenv_iac::no_config))]
    NoConfigLoaded,

    /// Configuration file not found.
    #[error("Configuration file not found: {path}")]
    #[diagnostic(code(cuenv_iac::config_not_found))]
    ConfigNotFound {
        /// Path to the missing file
        path: PathBuf,
    },

    /// Failed to parse CUE configuration.
    #[error("Failed to parse CUE configuration: {message}")]
    #[diagnostic(code(cuenv_iac::cue_parse_error))]
    CueParse {
        /// Error message from CUE
        message: String,
        /// Path to the CUE file
        #[source_code]
        path: Option<PathBuf>,
    },

    /// Cyclic dependency detected in resource graph.
    #[error("Cyclic dependency detected in resource graph")]
    #[diagnostic(
        code(cuenv_iac::cyclic_dependency),
        help("Review resource dependencies to break the cycle")
    )]
    CyclicDependency,

    /// Resource not found.
    #[error("Resource not found: {resource_id}")]
    #[diagnostic(code(cuenv_iac::resource_not_found))]
    ResourceNotFound {
        /// Resource identifier
        resource_id: String,
    },

    /// Provider not found.
    #[error("Provider not found: {provider_name}")]
    #[diagnostic(code(cuenv_iac::provider_not_found))]
    ProviderNotFound {
        /// Provider name
        provider_name: String,
    },

    /// Provider failed to start.
    #[error("Provider failed to start: {provider_name}: {message}")]
    #[diagnostic(code(cuenv_iac::provider_start_failed))]
    ProviderStartFailed {
        /// Provider name
        provider_name: String,
        /// Error message
        message: String,
    },

    /// Provider handshake failed.
    #[error("Provider handshake failed: {message}")]
    #[diagnostic(
        code(cuenv_iac::provider_handshake_failed),
        help("Ensure the provider binary is compatible with tfplugin6 protocol")
    )]
    ProviderHandshakeFailed {
        /// Error message
        message: String,
    },

    /// gRPC communication error.
    #[error("gRPC error: {0}")]
    #[diagnostic(code(cuenv_iac::grpc_error))]
    Grpc(#[from] tonic::Status),

    /// gRPC transport error.
    #[error("gRPC transport error: {0}")]
    #[diagnostic(code(cuenv_iac::grpc_transport_error))]
    GrpcTransport(#[from] tonic::transport::Error),

    /// Actor system error.
    #[error("Actor error: {message}")]
    #[diagnostic(code(cuenv_iac::actor_error))]
    Actor {
        /// Error message
        message: String,
    },

    /// Actor spawn error.
    #[error("Failed to spawn actor: {0}")]
    #[diagnostic(code(cuenv_iac::actor_spawn_error))]
    ActorSpawn(#[from] ractor::SpawnErr),

    /// Actor messaging error.
    #[error("Actor messaging error: {0}")]
    #[diagnostic(code(cuenv_iac::actor_messaging_error))]
    ActorMessaging(#[from] ractor::MessagingErr<()>),

    /// Resource operation failed.
    #[error("Resource operation failed for {resource_id}: {message}")]
    #[diagnostic(code(cuenv_iac::resource_operation_failed))]
    ResourceOperationFailed {
        /// Resource identifier
        resource_id: String,
        /// Error message
        message: String,
    },

    /// Resource state invalid.
    #[error("Invalid resource state for {resource_id}: expected {expected}, got {actual}")]
    #[diagnostic(code(cuenv_iac::invalid_resource_state))]
    InvalidResourceState {
        /// Resource identifier
        resource_id: String,
        /// Expected state
        expected: String,
        /// Actual state
        actual: String,
    },

    /// Drift detection is disabled.
    #[error("Drift detection is disabled")]
    #[diagnostic(
        code(cuenv_iac::drift_detection_disabled),
        help("Enable drift detection in IacSystemConfig")
    )]
    DriftDetectionDisabled,

    /// Drift detected.
    #[error("Drift detected for {resource_id}: {description}")]
    #[diagnostic(code(cuenv_iac::drift_detected))]
    DriftDetected {
        /// Resource identifier
        resource_id: String,
        /// Description of the drift
        description: String,
    },

    /// Serialization error.
    #[error("Serialization error: {0}")]
    #[diagnostic(code(cuenv_iac::serialization_error))]
    Serialization(#[from] serde_json::Error),

    /// MessagePack serialization error.
    #[error("MessagePack serialization error: {0}")]
    #[diagnostic(code(cuenv_iac::msgpack_error))]
    MsgPack(String),

    /// IO error.
    #[error("IO error: {0}")]
    #[diagnostic(code(cuenv_iac::io_error))]
    Io(#[from] std::io::Error),

    /// TLS error.
    #[error("TLS error: {0}")]
    #[diagnostic(code(cuenv_iac::tls_error))]
    Tls(String),

    /// Timeout error.
    #[error("Operation timed out: {operation}")]
    #[diagnostic(code(cuenv_iac::timeout))]
    Timeout {
        /// Operation that timed out
        operation: String,
    },

    /// Provider returned diagnostics.
    #[error("Provider diagnostics: {0}")]
    #[diagnostic(code(cuenv_iac::provider_diagnostics))]
    ProviderDiagnostics(String),

    /// Invalid configuration.
    #[error("Invalid configuration: {message}")]
    #[diagnostic(code(cuenv_iac::invalid_config))]
    InvalidConfig {
        /// Error message
        message: String,
    },

    /// Dependency resolution failed.
    #[error("Failed to resolve dependency {dependency} for resource {resource_id}")]
    #[diagnostic(code(cuenv_iac::dependency_resolution_failed))]
    DependencyResolutionFailed {
        /// Resource identifier
        resource_id: String,
        /// Dependency that failed to resolve
        dependency: String,
    },
}

impl From<rmp_serde::encode::Error> for Error {
    fn from(err: rmp_serde::encode::Error) -> Self {
        Error::MsgPack(err.to_string())
    }
}

impl From<rmp_serde::decode::Error> for Error {
    fn from(err: rmp_serde::decode::Error) -> Self {
        Error::MsgPack(err.to_string())
    }
}

impl<T> From<ractor::MessagingErr<T>> for Error {
    fn from(err: ractor::MessagingErr<T>) -> Self {
        Error::Actor {
            message: format!("Messaging error: {err:?}"),
        }
    }
}

impl From<rcgen::Error> for Error {
    fn from(err: rcgen::Error) -> Self {
        Error::Tls(err.to_string())
    }
}
