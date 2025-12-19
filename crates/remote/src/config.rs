//! Configuration types for REAPI client

use cuenv_core::config::BackendOptions;
use serde::{Deserialize, Serialize};

/// Configuration for the REAPI remote backend
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteConfig {
    /// REAPI server endpoint (e.g., "grpcs://buildbarn.example.com:8980")
    pub endpoint: String,

    /// Instance name for multi-tenant REAPI servers
    #[serde(default = "default_instance_name")]
    pub instance_name: String,

    /// Authentication configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,

    /// Enable remote caching (ActionCache)
    #[serde(default = "default_true")]
    pub remote_cache: bool,

    /// Enable remote execution
    #[serde(default = "default_true")]
    pub remote_execution: bool,

    /// Compression settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<CompressionConfig>,

    /// Maximum concurrent blob uploads
    #[serde(default = "default_max_concurrent_uploads")]
    pub max_concurrent_uploads: usize,

    /// Maximum concurrent task executions
    #[serde(default = "default_max_concurrent_executions")]
    pub max_concurrent_executions: usize,

    /// Operation timeout in seconds
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Retry configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    /// Secrets handling mode
    #[serde(default)]
    pub secrets: SecretsMode,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            instance_name: default_instance_name(),
            auth: None,
            remote_cache: true,
            remote_execution: true,
            compression: None,
            max_concurrent_uploads: default_max_concurrent_uploads(),
            max_concurrent_executions: default_max_concurrent_executions(),
            timeout_secs: default_timeout_secs(),
            retry: None,
            secrets: SecretsMode::default(),
        }
    }
}

impl RemoteConfig {
    /// Create a RemoteConfig from BackendOptions with pre-resolved auth
    ///
    /// The `resolved_auth` parameter should contain the resolved authentication
    /// configuration (secrets already resolved to plain strings).
    pub fn from_backend_options_with_auth(
        options: &BackendOptions,
        resolved_auth: Option<AuthConfig>,
    ) -> Self {
        Self {
            endpoint: options
                .endpoint
                .clone()
                .unwrap_or_else(|| "grpc://localhost:8980".to_string()),
            instance_name: options
                .instance_name
                .clone()
                .unwrap_or_else(default_instance_name),
            auth: resolved_auth,
            ..Default::default()
        }
    }

    /// Create a RemoteConfig from BackendOptions (deprecated - use from_backend_options_with_auth)
    ///
    /// This method does not support secret resolution. Use `from_backend_options_with_auth`
    /// with pre-resolved auth for full functionality.
    #[deprecated(
        since = "0.1.0",
        note = "Use from_backend_options_with_auth with resolved auth instead"
    )]
    pub fn from_backend_options(options: &BackendOptions) -> Self {
        Self::from_backend_options_with_auth(options, None)
    }
}

/// Authentication configuration (resolved, ready to use)
///
/// This enum holds resolved authentication values (no secrets - those should
/// be resolved before constructing this type).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthConfig {
    /// Bearer token authentication (Authorization: Bearer <token>)
    Bearer { token: String },

    /// BuildBuddy API key authentication (x-buildbuddy-api-key: <token>)
    #[serde(rename = "buildbuddy")]
    BuildBuddy {
        /// Resolved API key value
        #[serde(rename = "apiKey")]
        api_key: String,
    },

    /// mTLS authentication (future)
    #[serde(rename = "mtls")]
    MTls {
        cert_path: String,
        key_path: String,
        ca_path: Option<String>,
    },

    /// Google Cloud authentication (future)
    GoogleCloud,
}

/// Compression configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompressionConfig {
    /// Enable compression for uploads
    #[serde(default = "default_true")]
    pub upload: bool,

    /// Enable compression for downloads
    #[serde(default = "default_true")]
    pub download: bool,
}

/// Retry configuration with exponential backoff
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,

    /// Initial backoff duration in milliseconds
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,

    /// Maximum backoff duration in milliseconds
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,

    /// Backoff multiplier
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            initial_backoff_ms: default_initial_backoff_ms(),
            max_backoff_ms: default_max_backoff_ms(),
            backoff_multiplier: default_backoff_multiplier(),
        }
    }
}

/// Secrets handling mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum SecretsMode {
    /// Inline secrets in Command environment variables (default, works with all servers)
    #[default]
    Inline,

    /// Send secrets via BuildBuddy-specific headers (more secure, BuildBuddy only)
    Headers,
}

// Default value functions
fn default_instance_name() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

fn default_max_concurrent_uploads() -> usize {
    16
}

fn default_max_concurrent_executions() -> usize {
    8
}

fn default_timeout_secs() -> u64 {
    600 // 10 minutes
}

fn default_max_attempts() -> usize {
    3
}

fn default_initial_backoff_ms() -> u64 {
    100
}

fn default_max_backoff_ms() -> u64 {
    10000
}

fn default_backoff_multiplier() -> f64 {
    2.0
}
