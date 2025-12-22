//! Sync provider registry for managing and executing sync providers.

use super::super::CommandExecutor;
use super::provider::{SyncOptions, SyncProvider, SyncResult};
use cuenv_core::Result;
use std::path::Path;

/// Registry of sync providers.
///
/// Providers are registered at startup and can be queried by name or
/// executed collectively.
pub struct SyncRegistry {
    providers: Vec<Box<dyn SyncProvider>>,
}

impl Default for SyncRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Register a sync provider.
    pub fn register<P: SyncProvider + 'static>(&mut self, provider: P) {
        self.providers.push(Box::new(provider));
    }

    /// Get a provider by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn SyncProvider> {
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(AsRef::as_ref)
    }

    /// Get all registered providers.
    #[allow(dead_code)]
    pub fn all(&self) -> impl Iterator<Item = &dyn SyncProvider> {
        self.providers.iter().map(AsRef::as_ref)
    }

    /// Get provider names for CLI help.
    pub fn names(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Build CLI commands for all providers.
    #[allow(dead_code)]
    pub fn build_commands(&self) -> Vec<clap::Command> {
        self.providers.iter().map(|p| p.build_command()).collect()
    }

    /// Sync all providers.
    ///
    /// When `all_projects` is true, calls `sync_workspace` on each provider.
    /// Otherwise calls `sync_path` for the given path.
    pub async fn sync_all(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        all_projects: bool,
        executor: &CommandExecutor,
    ) -> Result<String> {
        let mut outputs = Vec::new();
        let mut had_error = false;

        for provider in &self.providers {
            let result = if all_projects {
                provider.sync_workspace(package, options, executor).await
            } else {
                provider.sync_path(path, package, options, executor).await
            };

            match result {
                Ok(r) => {
                    if !r.output.is_empty() {
                        outputs.push(format!("[{}]\n{}", provider.name(), r.output));
                    }
                    had_error |= r.had_error;
                }
                Err(e) => {
                    outputs.push(format!("[{}] Error: {}", provider.name(), e));
                    had_error = true;
                }
            }
        }

        let combined = outputs.join("\n\n");

        if had_error {
            Err(cuenv_core::Error::configuration(combined))
        } else if combined.is_empty() {
            Ok("No sync operations performed.".to_string())
        } else {
            Ok(combined)
        }
    }

    /// Sync a specific provider by name.
    pub async fn sync_provider(
        &self,
        name: &str,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        all_projects: bool,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let provider = self.get(name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Unknown sync provider: '{}'. Available: {}",
                name,
                self.names().join(", ")
            ))
        })?;

        if all_projects {
            provider.sync_workspace(package, options, executor).await
        } else {
            provider.sync_path(path, package, options, executor).await
        }
    }
}
