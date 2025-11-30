//! Configuration types for cuenv
//!
//! Based on schema/config.cue

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Main configuration structure for cuenv
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
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

    /// Backend configuration for task execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendConfig>,
}

/// Task output format options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Tui,
    Spinner,
    Simple,
    Tree,
    Json,
}

/// Cache mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheMode {
    Off,
    Read,
    ReadWrite,
    Write,
}

/// Backend configuration for task execution
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BackendConfig {
    /// Which backend to use by default for tasks ("host" or "dagger")
    #[serde(default = "default_backend_type")]
    #[serde(rename = "type")]
    pub backend_type: BackendType,

    /// Backend-specific default options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<BackendOptions>,
}

fn default_backend_type() -> BackendType {
    BackendType::Host
}

/// The type of backend to use for task execution
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendType {
    /// Run tasks directly on the host machine (default)
    #[default]
    Host,
    /// Run tasks inside Dagger containers
    Dagger,
}

/// Backend-specific options
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BackendOptions {
    /// Container image for Dagger backend (e.g., "ubuntu:22.04")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Optional platform specification (e.g., "linux/amd64")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_config_default() {
        let config: BackendConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.backend_type, BackendType::Host);
        assert!(config.options.is_none());
    }

    #[test]
    fn test_backend_config_host() {
        let json = r#"{"type": "host"}"#;
        let config: BackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend_type, BackendType::Host);
    }

    #[test]
    fn test_backend_config_dagger() {
        let json = r#"{"type": "dagger", "options": {"image": "ubuntu:22.04"}}"#;
        let config: BackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend_type, BackendType::Dagger);
        assert_eq!(
            config.options.unwrap().image,
            Some("ubuntu:22.04".to_string())
        );
    }

    #[test]
    fn test_backend_type_default() {
        let backend_type = BackendType::default();
        assert_eq!(backend_type, BackendType::Host);
    }

    #[test]
    fn test_config_with_backend() {
        let json = r#"{
            "backend": {
                "type": "dagger",
                "options": {
                    "image": "alpine:latest",
                    "platform": "linux/amd64"
                }
            }
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        let backend = config.backend.unwrap();
        assert_eq!(backend.backend_type, BackendType::Dagger);
        let options = backend.options.unwrap();
        assert_eq!(options.image, Some("alpine:latest".to_string()));
        assert_eq!(options.platform, Some("linux/amd64".to_string()));
    }
}
