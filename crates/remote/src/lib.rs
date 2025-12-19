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
