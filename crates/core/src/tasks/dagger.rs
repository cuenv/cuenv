//! Legacy task-level Dagger configuration types.

use serde::{Deserialize, Serialize};

/// Dagger-specific task configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DaggerTaskConfig {
    /// Base container image for running the task (e.g., "ubuntu:22.04")
    /// Overrides the global backend.options.image if set.
    #[serde(default)]
    pub image: Option<String>,

    /// Use container from a previous task as base instead of an image.
    /// The referenced task must have run first (use dependsOn to ensure ordering).
    #[serde(default)]
    pub from: Option<String>,

    /// Secrets to mount or expose as environment variables.
    /// Secrets are resolved using cuenv's secret resolvers and securely passed to Dagger.
    #[serde(default)]
    pub secrets: Option<Vec<DaggerSecret>>,

    /// Cache volumes to mount for persistent build caching.
    #[serde(default)]
    pub cache: Option<Vec<DaggerCacheMount>>,
}

/// Secret configuration for Dagger containers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaggerSecret {
    /// Name identifier for the secret in Dagger
    pub name: String,

    /// Mount secret as a file at this path (e.g., "/root/.npmrc")
    #[serde(default)]
    pub path: Option<String>,

    /// Expose secret as an environment variable with this name
    #[serde(default, rename = "envVar")]
    pub env_var: Option<String>,

    /// Secret resolver configuration
    pub resolver: crate::secrets::Secret,
}

/// Cache volume mount configuration for Dagger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaggerCacheMount {
    /// Path inside the container to mount the cache (e.g., "/root/.npm")
    pub path: String,

    /// Unique name for the cache volume
    pub name: String,
}
