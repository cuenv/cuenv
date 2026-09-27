//! Error type for the infrastructure engine.

use thiserror::Error;

/// Result alias for this crate.
pub type Result<Success> = std::result::Result<Success, InfrastructureError>;

/// Errors raised while planning or applying infrastructure.
#[derive(Debug, Error)]
pub enum InfrastructureError {
    /// Invalid `infrastructure` configuration.
    #[error("infrastructure configuration error: {0}")]
    Configuration(String),

    /// Failure encoding or decoding Terraform values.
    #[error("value codec error: {0}")]
    Codec(String),

    /// Failure launching or talking to a provider plugin process.
    #[error("provider plugin error: {0}")]
    Plugin(String),

    /// A gRPC call to a provider failed.
    #[error("provider RPC {method} failed: {status}")]
    RemoteProcedure {
        /// RPC method name.
        method: String,
        /// gRPC status returned by the provider.
        status: Box<tonic::Status>,
    },

    /// A provider returned error diagnostics.
    #[error("{context}:\n{}", .errors.join("\n"))]
    Diagnostics {
        /// What cuenv was doing when the provider failed.
        context: String,
        /// Rendered error diagnostics.
        errors: Vec<String>,
    },

    /// Failure downloading or installing a provider from a registry.
    #[error("provider install error: {0}")]
    Install(String),

    /// State storage failure.
    #[error("state store error: {0}")]
    State(String),

    /// The tenant's state is locked by another run.
    #[error(
        "state for {tenant} is locked by '{holder}' (lock {lock_identifier}, acquired {acquired_at}); \
         run `cuenv infrastructure unlock` if that run is gone"
    )]
    Locked {
        /// Tenant whose state is locked.
        tenant: String,
        /// Lock identifier.
        lock_identifier: String,
        /// Description of the lock holder.
        holder: String,
        /// When the lock was acquired.
        acquired_at: String,
    },

    /// I/O failure.
    #[error("{context}: {source}")]
    InputOutput {
        /// What cuenv was doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl InfrastructureError {
    /// Build a configuration error.
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Build a codec error.
    pub fn codec(message: impl Into<String>) -> Self {
        Self::Codec(message.into())
    }

    /// Build a plugin error.
    pub fn plugin(message: impl Into<String>) -> Self {
        Self::Plugin(message.into())
    }

    /// Build an install error.
    pub fn install(message: impl Into<String>) -> Self {
        Self::Install(message.into())
    }

    /// Build a state store error.
    pub fn state(message: impl Into<String>) -> Self {
        Self::State(message.into())
    }

    /// Build an I/O error with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::InputOutput {
            context: context.into(),
            source,
        }
    }
}
