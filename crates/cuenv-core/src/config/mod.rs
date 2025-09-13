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
}

/// Task output format options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Tui,
    Spinner,
    Simple,
    Tree,
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
