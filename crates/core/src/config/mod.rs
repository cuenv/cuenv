//! Configuration types for cuenv
//!
//! Based on schema/config.cue

use crate::secrets::Secret;
use serde::{Deserialize, Serialize};

/// Main configuration structure for cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Task output format
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,

    /// Cache configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_mode: Option<CacheMode>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,

    /// Security and debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_mode: Option<bool>,

    /// Chrome trace generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_output: Option<bool>,

    /// Default environment settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_capabilities: Option<Vec<String>>,

    /// Task backend configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendConfig>,
}

/// Task output format options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Tui,
    Spinner,
    Simple,
    Tree,
    Json,
}

/// Cache mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheMode {
    Off,
    Read,
    ReadWrite,
    Write,
}

fn default_backend_type() -> String {
    "host".to_string()
}

/// Backend configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackendConfig {
    /// Which backend to use for tasks
    #[serde(default = "default_backend_type", rename = "type")]
    pub backend_type: String,

    /// Backend-specific options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<BackendOptions>,
}

/// Backend-specific options supported by cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackendOptions {
    /// Default container image for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Optional platform hint for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// REAPI server endpoint (e.g., "grpcs://buildbarn.example.com:8980")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Instance name for multi-tenant REAPI servers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,

    /// Authentication configuration for remote backends
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<BackendAuth>,
}

/// Authentication configuration for backend services
///
/// Supports multiple auth types with optional secret resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BackendAuth {
    /// Bearer token authentication (Authorization: Bearer <token>)
    Bearer {
        /// Token value (plain string or secret reference)
        token: StringOrSecret,
    },

    /// BuildBuddy API key authentication (x-buildbuddy-api-key: <token>)
    #[serde(rename = "buildbuddy")]
    BuildBuddy {
        /// API key value (plain string or secret reference)
        #[serde(rename = "apiKey")]
        api_key: StringOrSecret,
    },

    /// mTLS authentication
    #[serde(rename = "mtls")]
    MTls {
        /// Path to client certificate
        #[serde(rename = "certPath")]
        cert_path: String,
        /// Path to client private key
        #[serde(rename = "keyPath")]
        key_path: String,
        /// Optional path to CA certificate
        #[serde(rename = "caPath")]
        ca_path: Option<String>,
    },
}

/// A value that can be either a plain string or a secret reference
///
/// When serialized from CUE, a plain string comes as `String` variant,
/// while a `#Secret` or `#OnePasswordRef` comes as the `Secret` variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StringOrSecret {
    /// Plain string value (no secret resolution needed)
    Plain(String),
    /// Secret reference that needs async resolution
    Secret(Secret),
}
