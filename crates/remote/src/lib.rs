//! REAPI (Remote Execution API) backend for cuenv
//!
//! This crate implements a client for the Bazel Remote Execution API v2,
//! enabling distributed task execution across remote workers.

pub mod client;
pub mod config;
pub mod error;
pub mod mapper;
pub mod merkle;
pub mod retry;

mod backend;

pub use backend::RemoteBackend;
pub use config::{AuthConfig, CompressionConfig, RemoteConfig, RetryConfig, SecretsMode};
pub use error::{RemoteError, Result};

use cuenv_core::config::BackendConfig;
use cuenv_core::tasks::TaskBackend;
use std::path::PathBuf;
use std::sync::Arc;

/// Create a remote backend from configuration
pub fn create_remote_backend(
    config: Option<&BackendConfig>,
    project_root: PathBuf,
) -> Arc<dyn TaskBackend> {
    let remote_config = config
        .and_then(|c| c.options.as_ref())
        .map(RemoteConfig::from_backend_options)
        .unwrap_or_default();

    Arc::new(RemoteBackend::new(remote_config, project_root))
}
