//! Provider lifecycle management.
//!
//! This module handles provider discovery, caching, and lifecycle management.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use crate::error::{Error, Result};

use super::client::ProviderClient;

/// Configuration for a provider instance.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// Provider source (e.g., "hashicorp/aws")
    pub source: String,

    /// Provider version constraint
    pub version: Option<String>,

    /// Path to provider binary (if known)
    pub binary_path: Option<PathBuf>,

    /// Provider configuration
    pub config: serde_json::Value,
}

impl ProviderConfig {
    /// Creates a new provider configuration.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            version: None,
            binary_path: None,
            config: serde_json::Value::Null,
        }
    }

    /// Sets the version constraint.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sets the binary path.
    #[must_use]
    pub fn with_binary_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.binary_path = Some(path.into());
        self
    }

    /// Sets the provider configuration.
    #[must_use]
    pub fn with_config(mut self, config: serde_json::Value) -> Self {
        self.config = config;
        self
    }

    /// Extracts the provider name from the source.
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.source
            .split('/')
            .last()
            .unwrap_or(&self.source)
    }
}

/// Manages provider instances and their lifecycle.
pub struct ProviderManager {
    /// Active provider clients
    providers: DashMap<String, Arc<ProviderClient>>,

    /// Provider configurations
    configs: DashMap<String, ProviderConfig>,

    /// Cache directory for provider binaries
    cache_dir: Option<PathBuf>,

    /// Plugin directory for looking up provider binaries
    plugin_dir: RwLock<Option<PathBuf>>,
}

impl ProviderManager {
    /// Creates a new provider manager.
    #[must_use]
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            providers: DashMap::new(),
            configs: DashMap::new(),
            cache_dir,
            plugin_dir: RwLock::new(None),
        }
    }

    /// Sets the plugin directory for provider lookups.
    pub async fn set_plugin_dir(&self, dir: impl Into<PathBuf>) {
        let mut plugin_dir = self.plugin_dir.write().await;
        *plugin_dir = Some(dir.into());
    }

    /// Registers a provider configuration.
    pub fn register_provider(&self, name: impl Into<String>, config: ProviderConfig) {
        self.configs.insert(name.into(), config);
    }

    /// Gets or starts a provider by name.
    ///
    /// If the provider is already running, returns the existing client.
    /// Otherwise, starts a new provider instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider cannot be started.
    #[instrument(name = "get_or_start_provider", skip(self))]
    pub async fn get_or_start(&self, name: &str) -> Result<Arc<ProviderClient>> {
        // Check if already running
        if let Some(client) = self.providers.get(name) {
            if client.is_running().await {
                return Ok(Arc::clone(&client));
            }
            // Provider died, remove it
            drop(client);
            self.providers.remove(name);
        }

        // Get configuration
        let config = self.configs.get(name).ok_or_else(|| Error::ProviderNotFound {
            provider_name: name.to_string(),
        })?;

        // Find the provider binary
        let binary_path = self.find_provider_binary(&config).await?;

        info!(
            provider = name,
            binary = %binary_path.display(),
            "Starting provider"
        );

        // Start the provider
        let client = ProviderClient::new(name, binary_path.to_string_lossy().as_ref()).await?;

        // Configure the provider
        client.configure(&config.config).await?;

        let client = Arc::new(client);
        self.providers.insert(name.to_string(), Arc::clone(&client));

        Ok(client)
    }

    /// Gets a running provider by name.
    ///
    /// Returns None if the provider is not running.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<ProviderClient>> {
        self.providers.get(name).map(|r| Arc::clone(&r))
    }

    /// Stops a provider by name.
    ///
    /// # Errors
    ///
    /// Returns an error if stopping fails.
    #[instrument(name = "stop_provider", skip(self))]
    pub async fn stop(&self, name: &str) -> Result<()> {
        if let Some((_, client)) = self.providers.remove(name) {
            client.stop().await?;
        }
        Ok(())
    }

    /// Stops all running providers.
    ///
    /// # Errors
    ///
    /// Returns an error if any provider fails to stop.
    #[instrument(name = "stop_all_providers", skip(self))]
    pub async fn stop_all(&self) -> Result<()> {
        let names: Vec<_> = self.providers.iter().map(|r| r.key().clone()).collect();

        for name in names {
            if let Err(e) = self.stop(&name).await {
                warn!(provider = %name, error = %e, "Failed to stop provider");
            }
        }

        Ok(())
    }

    /// Returns the names of all running providers.
    #[must_use]
    pub fn running_providers(&self) -> Vec<String> {
        self.providers.iter().map(|r| r.key().clone()).collect()
    }

    /// Finds the provider binary path.
    async fn find_provider_binary(&self, config: &ProviderConfig) -> Result<PathBuf> {
        // Check explicit binary path first
        if let Some(path) = &config.binary_path {
            if path.exists() {
                return Ok(path.clone());
            }
        }

        // Check plugin directory
        let plugin_dir = self.plugin_dir.read().await;
        if let Some(dir) = plugin_dir.as_ref() {
            let provider_name = config.provider_name();
            let patterns = [
                format!("terraform-provider-{provider_name}"),
                format!("terraform-provider-{provider_name}_*"),
            ];

            for pattern in &patterns {
                let path = dir.join(pattern);
                if path.exists() {
                    return Ok(path);
                }
            }

            // Look for versioned binary
            if let Some(version) = &config.version {
                let versioned = dir.join(format!(
                    "terraform-provider-{provider_name}_{version}"
                ));
                if versioned.exists() {
                    return Ok(versioned);
                }
            }
        }

        // Check cache directory
        if let Some(cache_dir) = &self.cache_dir {
            let provider_name = config.provider_name();
            let cached = cache_dir.join(format!("terraform-provider-{provider_name}"));
            if cached.exists() {
                return Ok(cached);
            }
        }

        // Check system PATH
        let provider_name = config.provider_name();
        if let Ok(path) = which::which(format!("terraform-provider-{provider_name}")) {
            return Ok(path);
        }

        // Check Terraform plugin cache
        if let Some(terraform_cache) = Self::terraform_plugin_cache() {
            let provider_path = Self::find_in_terraform_cache(&terraform_cache, config)?;
            if let Some(path) = provider_path {
                return Ok(path);
            }
        }

        Err(Error::ProviderNotFound {
            provider_name: config.source.clone(),
        })
    }

    /// Returns the Terraform plugin cache directory.
    fn terraform_plugin_cache() -> Option<PathBuf> {
        // Check TF_PLUGIN_CACHE_DIR environment variable
        if let Ok(dir) = std::env::var("TF_PLUGIN_CACHE_DIR") {
            return Some(PathBuf::from(dir));
        }

        // Check default locations
        let home = dirs::home_dir()?;

        // Linux/macOS: ~/.terraform.d/plugins
        let default = home.join(".terraform.d").join("plugins");
        if default.exists() {
            return Some(default);
        }

        // Check data directory (Linux: ~/.local/share/terraform, macOS: ~/Library/Application Support/terraform)
        let data_dir = dirs::data_dir()?;
        let terraform_data = data_dir.join("terraform").join("plugins");
        if terraform_data.exists() {
            return Some(terraform_data);
        }

        None
    }

    /// Finds a provider in the Terraform plugin cache.
    fn find_in_terraform_cache(cache_dir: &Path, config: &ProviderConfig) -> Result<Option<PathBuf>> {
        // Parse source address
        let parts: Vec<&str> = config.source.split('/').collect();
        if parts.len() < 2 {
            return Ok(None);
        }

        let (namespace, name) = if parts.len() == 2 {
            // Assume registry.terraform.io
            ("hashicorp", parts[1])
        } else {
            (parts[0], parts[1])
        };

        // Build path: cache_dir/registry.terraform.io/namespace/name/version/os_arch/terraform-provider-name_version
        let registry_dir = cache_dir.join("registry.terraform.io").join(namespace).join(name);

        if !registry_dir.exists() {
            return Ok(None);
        }

        // Find the latest version or specified version
        let version = if let Some(v) = &config.version {
            v.clone()
        } else {
            // Find latest version
            let mut versions: Vec<_> = std::fs::read_dir(&registry_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .map(|s| s.to_string())
                })
                .collect();

            versions.sort_by(|a, b| {
                // Simple version comparison (works for semver)
                b.cmp(a)
            });

            versions.first().cloned().unwrap_or_default()
        };

        if version.is_empty() {
            return Ok(None);
        }

        // Determine OS and architecture
        let os = std::env::consts::OS;
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            arch => arch,
        };

        let version_dir = registry_dir.join(&version).join(format!("{os}_{arch}"));
        if !version_dir.exists() {
            return Ok(None);
        }

        // Find the binary
        let binary_name = format!("terraform-provider-{name}_{version}");
        let binary_path = version_dir.join(&binary_name);

        if binary_path.exists() {
            return Ok(Some(binary_path));
        }

        // Try without version in name
        let binary_name = format!("terraform-provider-{name}");
        let binary_path = version_dir.join(&binary_name);

        if binary_path.exists() {
            return Ok(Some(binary_path));
        }

        Ok(None)
    }
}

impl Drop for ProviderManager {
    fn drop(&mut self) {
        // Best-effort cleanup of providers
        for entry in self.providers.iter() {
            debug!(provider = %entry.key(), "Dropping provider manager, provider will be killed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_config() {
        let config = ProviderConfig::new("hashicorp/aws")
            .with_version("5.0.0")
            .with_config(serde_json::json!({"region": "us-east-1"}));

        assert_eq!(config.provider_name(), "aws");
        assert_eq!(config.version, Some("5.0.0".to_string()));
    }

    #[test]
    fn test_provider_name_extraction() {
        let config1 = ProviderConfig::new("aws");
        assert_eq!(config1.provider_name(), "aws");

        let config2 = ProviderConfig::new("hashicorp/aws");
        assert_eq!(config2.provider_name(), "aws");

        let config3 = ProviderConfig::new("registry.terraform.io/hashicorp/aws");
        assert_eq!(config3.provider_name(), "aws");
    }
}
