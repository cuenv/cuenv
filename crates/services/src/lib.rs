//! Service supervision, readiness probes, and process management for cuenv.
//!
//! This crate provides the runtime behavior for long-running supervised
//! processes defined via `#Service` in CUE configuration. It implements:
//!
//! - Lifecycle state machine (Pending -> Starting -> Ready -> Stopped/Failed)
//! - Readiness probes (port, HTTP, log pattern, command, delay)
//! - Restart supervision with exponential backoff
//! - File watching for restart-on-change
//! - Session state management under `.cuenv/run/`
//! - Top-level service controller for `cuenv up` orchestration

pub mod controller;
pub mod duration;
pub mod lifecycle;
pub mod probes;
pub mod session;
pub mod supervisor;
pub mod watcher;

use miette::Diagnostic;
use thiserror::Error;

/// Error type for service operations.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// A service failed to start or crashed.
    #[error("Service '{name}' failed: {message}")]
    #[diagnostic(code(cuenv::service::failed))]
    ServiceFailed {
        /// Service name.
        name: String,
        /// Error description.
        message: String,
        /// Help text.
        #[help]
        help: Option<String>,
    },

    /// Session state error (lock, read, write).
    #[error("Session error: {message}")]
    #[diagnostic(code(cuenv::session::error))]
    Session {
        /// Error description.
        message: String,
        /// Help text.
        #[help]
        help: Option<String>,
    },

    /// Readiness probe failed or timed out.
    #[error("Readiness probe failed for '{name}': {message}")]
    #[diagnostic(code(cuenv::service::probe))]
    ProbeFailed {
        /// Service name.
        name: String,
        /// Error description.
        message: String,
    },

    /// Invalid lifecycle state transition.
    #[error("Invalid state transition for '{name}': {from} -> {to}")]
    #[diagnostic(code(cuenv::service::lifecycle))]
    InvalidTransition {
        /// Service name.
        name: String,
        /// Current state.
        from: String,
        /// Attempted target state.
        to: String,
    },

    /// Duration parsing error.
    #[error("Invalid duration '{input}': {message}")]
    #[diagnostic(
        code(cuenv::service::duration),
        help("Use formats like \"500ms\", \"10s\", \"1m\", \"1h\"")
    )]
    InvalidDuration {
        /// The input string that failed to parse.
        input: String,
        /// Error description.
        message: String,
    },

    /// Propagated core error.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Core(#[from] cuenv_core::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    #[diagnostic(code(cuenv::service::io))]
    Io(#[from] std::io::Error),

    /// Task graph error.
    #[error("Task graph error: {0}")]
    #[diagnostic(code(cuenv::service::graph))]
    TaskGraph(#[from] cuenv_task_graph::Error),
}

impl Error {
    /// Create a service-failed error.
    #[must_use]
    pub fn service_failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ServiceFailed {
            name: name.into(),
            message: message.into(),
            help: None,
        }
    }

    /// Create a session error.
    #[must_use]
    pub fn session(message: impl Into<String>) -> Self {
        Self::Session {
            message: message.into(),
            help: None,
        }
    }

    /// Create a probe-failed error.
    #[must_use]
    pub fn probe_failed(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ProbeFailed {
            name: name.into(),
            message: message.into(),
        }
    }
}

/// Result type for service operations.
pub type Result<T> = std::result::Result<T, Error>;
