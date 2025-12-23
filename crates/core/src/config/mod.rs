//! Configuration types for cuenv
//!
//! Based on schema/config.cue

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

    /// CI-specific configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CIConfig>,
}

/// CI-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CIConfig {
    /// Cuenv installation configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuenv: Option<CuenvConfig>,
}

/// Configuration for cuenv installation in CI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CuenvConfig {
    /// Source for cuenv binary
    #[serde(default)]
    pub source: CuenvSource,

    /// Version to install ("self", "latest", or specific version like "0.17.0")
    #[serde(default = "default_cuenv_version")]
    pub version: String,
}

fn default_cuenv_version() -> String {
    "self".to_string()
}

impl Default for CuenvConfig {
    fn default() -> Self {
        Self {
            source: CuenvSource::Release,
            version: default_cuenv_version(),
        }
    }
}

/// Source for cuenv binary in CI environments
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CuenvSource {
    /// Build from git checkout (requires Nix)
    Git,
    /// Install via Nix flake (auto-configures Cachix)
    Nix,
    /// Install via Homebrew tap (no Nix required)
    Homebrew,
    /// Download from GitHub Releases (default)
    #[default]
    Release,
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
}
