//! REAPI (Remote Execution API) backend for cuenv
//!
//! This crate implements a client for the Bazel Remote Execution API v2,
//! enabling distributed task execution across remote workers.

pub mod client;
pub mod config;
pub mod error;
pub mod mapper;
pub mod merkle;
pub mod nix;
pub mod retry;

mod backend;

/// Generated protobuf types from REAPI protos
pub mod proto {
    /// Bazel Remote Execution API v2 types
    pub mod build {
        pub mod bazel {
            pub mod remote {
                pub mod execution {
                    pub mod v2 {
                        tonic::include_proto!("build.bazel.remote.execution.v2");
                    }
                }
            }
            pub mod semver {
                tonic::include_proto!("build.bazel.semver");
            }
        }
    }

    /// Google API types
    pub mod google {
        pub mod bytestream {
            tonic::include_proto!("google.bytestream");
        }
        pub mod longrunning {
            tonic::include_proto!("google.longrunning");
        }
        pub mod rpc {
            tonic::include_proto!("google.rpc");
        }
    }
}

// Type aliases for convenience
pub use proto::build::bazel::remote::execution::v2 as reapi;

/// REAPI Digest type (SHA256 hash + size)
pub type ReapiDigest = reapi::Digest;
/// REAPI Command type
pub type ReapiCommand = reapi::Command;
/// REAPI Action type
pub type ReapiAction = reapi::Action;
/// REAPI ActionResult type
pub type ReapiActionResult = reapi::ActionResult;
/// REAPI Directory type
pub type ReapiDirectory = reapi::Directory;

pub use backend::RemoteBackend;
pub use config::{AuthConfig, CompressionConfig, RemoteConfig, RetryConfig, SecretsMode};
pub use error::{RemoteError, Result};

use cuenv_core::config::{BackendAuth, BackendConfig, StringOrSecret};
use cuenv_core::tasks::TaskBackend;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::debug;

/// Resolve a `StringOrSecret` value to a plain string
async fn resolve_string_or_secret(value: &StringOrSecret) -> Result<String> {
    match value {
        StringOrSecret::Plain(s) => Ok(s.clone()),
        StringOrSecret::Secret(secret) => {
            debug!(command = %secret.command, "Resolving secret");
            secret
                .resolve()
                .await
                .map_err(|e| RemoteError::config_error(format!("Failed to resolve secret: {}", e)))
        }
    }
}

/// Resolve `BackendAuth` to `AuthConfig` by resolving any secrets
async fn resolve_backend_auth(auth: &BackendAuth) -> Result<AuthConfig> {
    match auth {
        BackendAuth::Bearer { token } => {
            let resolved = resolve_string_or_secret(token).await?;
            Ok(AuthConfig::Bearer { token: resolved })
        }
        BackendAuth::BuildBuddy { api_key } => {
            let resolved = resolve_string_or_secret(api_key).await?;
            Ok(AuthConfig::BuildBuddy { api_key: resolved })
        }
        BackendAuth::MTls {
            cert_path,
            key_path,
            ca_path,
        } => Ok(AuthConfig::MTls {
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
            ca_path: ca_path.clone(),
        }),
    }
}

/// Create a remote backend from configuration (async version)
///
/// This is an async function because it may need to resolve secrets
/// (e.g., 1Password references) before creating the backend.
///
/// Nix packages are read from `config.options.nix_packages`, which should be
/// populated by the caller from `project.packages.nix` before calling this function.
pub async fn create_remote_backend_async(
    config: Option<&BackendConfig>,
    project_root: PathBuf,
) -> Result<Arc<dyn TaskBackend>> {
    let options = config.and_then(|c| c.options.as_ref());

    // Resolve authentication if configured
    let resolved_auth = match options.and_then(|o| o.auth.as_ref()) {
        Some(auth) => {
            debug!("Resolving backend authentication");
            Some(resolve_backend_auth(auth).await?)
        }
        None => None,
    };

    let remote_config = options
        .map(|opts| RemoteConfig::from_backend_options_with_auth(opts, resolved_auth.clone()))
        .unwrap_or_else(|| RemoteConfig {
            auth: resolved_auth,
            ..Default::default()
        });

    if !remote_config.nix_packages.is_empty() {
        debug!(
            package_count = remote_config.nix_packages.len(),
            "Remote backend configured with explicit Nix packages"
        );
    }

    Ok(Arc::new(RemoteBackend::new(remote_config, project_root)))
}

/// Create a remote backend from configuration (sync version)
///
/// This is the synchronous factory function that matches the `BackendFactory` type.
/// It uses `block_in_place` to resolve secrets when called from within an async context.
///
/// Nix packages are read from `config.options.nix_packages`, which should be
/// populated by the caller from `project.packages.nix` before calling this function.
///
/// # Panics
///
/// Panics if secret resolution fails. For better error handling, use
/// `create_remote_backend_async` directly in async code.
pub fn create_remote_backend(
    config: Option<&BackendConfig>,
    project_root: PathBuf,
) -> Arc<dyn TaskBackend> {
    // Use block_in_place to run async code within the current runtime
    // This is safe because we're called from within an async context (execute_task)
    tokio::task::block_in_place(|| {
        let handle = tokio::runtime::Handle::current();
        handle.block_on(async {
            match create_remote_backend_async(config, project_root).await {
                Ok(backend) => backend,
                Err(e) => {
                    // Log error and fall back to a non-functional backend that will
                    // report errors on actual use
                    tracing::error!(error = %e, "Failed to create remote backend");
                    panic!("Failed to create remote backend: {}", e);
                }
            }
        })
    })
}
