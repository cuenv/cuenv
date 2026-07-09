//! Sync provider registry for managing and executing sync providers.

use super::provider::{SyncProvider, SyncRequest, SyncResult};
use cuenv_core::Result;

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
    fn get(&self, name: &str) -> Option<&dyn SyncProvider> {
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(AsRef::as_ref)
    }

    fn names(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    pub async fn sync_selected(
        &self,
        provider_names: &[&str],
        request: SyncRequest<'_>,
    ) -> Result<String> {
        let mut outputs = Vec::new();
        let mut had_error = false;

        for name in provider_names {
            let result = self.sync_provider(name, request).await;
            match result {
                Ok(r) => {
                    if !r.output.is_empty() {
                        outputs.push(format!("[{name}]\n{}", r.output));
                    }
                    had_error |= r.had_error;
                }
                Err(e) => {
                    outputs.push(format!("[{name}] Error: {e}"));
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
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is not found or fails to sync.
    pub async fn sync_provider(&self, name: &str, request: SyncRequest<'_>) -> Result<SyncResult> {
        let provider = self.get(name).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Unknown sync provider: '{}'. Available: {}",
                name,
                self.names().join(", ")
            ))
        })?;

        provider.sync(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandExecutor;
    use crate::commands::sync::provider::{SyncOptions, SyncScope};
    use async_trait::async_trait;
    use std::path::Path;

    struct TestProvider {
        name: &'static str,
        result: Result<SyncResult>,
    }

    #[async_trait]
    impl SyncProvider for TestProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn sync(&self, _request: SyncRequest<'_>) -> Result<SyncResult> {
            match &self.result {
                Ok(result) => Ok(result.clone()),
                Err(error) => Err(cuenv_core::Error::configuration(error.to_string())),
            }
        }
    }

    fn executor() -> CommandExecutor {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        CommandExecutor::new(sender, "cuenv".to_string())
    }

    #[tokio::test]
    async fn dispatches_selected_providers_in_requested_order() {
        let mut registry = SyncRegistry::new();
        registry.register(TestProvider {
            name: "first",
            result: Ok(SyncResult::success("one")),
        });
        registry.register(TestProvider {
            name: "second",
            result: Ok(SyncResult::success("two")),
        });
        let executor = executor();
        let options = SyncOptions::default();
        let request = SyncRequest {
            path: Path::new("."),
            package: "cuenv",
            options: &options,
            scope: SyncScope::Path,
            executor: &executor,
        };

        let output = registry
            .sync_selected(&["second", "first"], request)
            .await
            .expect("selected providers should succeed");

        assert_eq!(output, "[second]\ntwo\n\n[first]\none");
    }

    #[tokio::test]
    async fn aggregates_provider_errors_with_provider_names() {
        let mut registry = SyncRegistry::new();
        registry.register(TestProvider {
            name: "ok",
            result: Ok(SyncResult::success("done")),
        });
        registry.register(TestProvider {
            name: "broken",
            result: Err(cuenv_core::Error::configuration("boom")),
        });
        let executor = executor();
        let options = SyncOptions::default();
        let request = SyncRequest {
            path: Path::new("."),
            package: "cuenv",
            options: &options,
            scope: SyncScope::Workspace,
            executor: &executor,
        };

        let error = registry
            .sync_selected(&["ok", "broken", "missing"], request)
            .await
            .expect_err("provider errors should fail the combined request");
        let message = error.to_string();

        assert!(message.contains("[ok]\ndone"));
        assert!(message.contains("[broken] Error:"));
        assert!(message.contains("boom"));
        assert!(message.contains("[missing] Error:"));
        assert!(message.contains("Unknown sync provider"));
    }
}
