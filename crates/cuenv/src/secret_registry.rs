//! Secret provider registration at the CLI composition root.
//!
//! The provider-backed resolvers (1Password, AWS, GCP, Infisical) are wired
//! here — not in `cuenv-core` — so library consumers of core don't pull in
//! the provider dependencies (including the 1Password extism WASM runtime).
//! `install` registers a factory with `cuenv_secrets`; the registry (and any
//! expensive provider initialization) is realized lazily on the first secret
//! resolution (RFC-0006 phase 3b).

use cuenv_secrets::{SecretError, SecretRegistry, SecretResolver, SecretSpec};
use std::sync::Arc;

/// Install the CLI's secret-registry factory into the process-wide registry.
///
/// Call once at startup, before any secret resolution. Cheap: providers are
/// initialized on first use, not here. If a registry was somehow already
/// installed or realized, the existing one wins and this is a no-op.
pub fn install() {
    let _ = cuenv_secrets::install_registry_factory(build_registry);
}

fn build_registry() -> SecretRegistry {
    let mut registry = SecretRegistry::with_builtins();

    #[cfg(feature = "1password")]
    register_or_stub(&mut registry, "onepassword", || {
        cuenv_1password::secrets::OnePasswordResolver::new()
            .map(|r| Arc::new(r) as Arc<dyn SecretResolver>)
            .map_err(|e| format!("Failed to initialize 1Password resolver: {e}"))
    });

    #[cfg(feature = "infisical")]
    register_or_stub(&mut registry, "infisical", || {
        cuenv_infisical::secrets::InfisicalResolver::new()
            .map(|r| Arc::new(r) as Arc<dyn SecretResolver>)
            .map_err(|e| format!("Failed to initialize Infisical resolver: {e}"))
    });

    #[cfg(feature = "aws")]
    registry.register(Arc::new(
        cuenv_aws::secrets::AwsSecretsManagerResolver::new(),
    ));

    #[cfg(feature = "gcp")]
    register_or_stub(&mut registry, "gcp", || {
        cuenv_gcp::secrets::GcpSecretManagerResolver::new()
            .map(|r| Arc::new(r) as Arc<dyn SecretResolver>)
            .map_err(|e| format!("Failed to initialize GCP resolver: {e}"))
    });

    registry
}

/// Register the resolver produced by `init`, or a stub that surfaces the
/// initialization failure at resolution time.
///
/// A broken provider must not take down resolution for every other provider
/// (the previous eager wiring failed the whole registry); users of the broken
/// provider still get the real initialization error when they try to use it.
#[cfg_attr(
    not(any(feature = "1password", feature = "infisical", feature = "gcp")),
    allow(dead_code)
)]
fn register_or_stub(
    registry: &mut SecretRegistry,
    provider: &'static str,
    init: impl FnOnce() -> Result<Arc<dyn SecretResolver>, String>,
) {
    match init() {
        Ok(resolver) => registry.register(resolver),
        Err(message) => registry.register(Arc::new(InitFailedResolver { provider, message })),
    }
}

/// Placeholder resolver that reports a provider's initialization failure
/// when a secret actually tries to use it.
struct InitFailedResolver {
    provider: &'static str,
    message: String,
}

#[async_trait::async_trait]
impl SecretResolver for InitFailedResolver {
    async fn resolve(&self, name: &str, _spec: &SecretSpec) -> Result<String, SecretError> {
        Err(SecretError::resolution_failed(name, self.message.clone()))
    }

    fn provider_name(&self) -> &'static str {
        self.provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_registry_has_builtins_and_default_providers() {
        let registry = build_registry();
        assert!(registry.has("env"));
        assert!(registry.has("exec"));

        #[cfg(feature = "1password")]
        assert!(registry.has("onepassword"));
        #[cfg(feature = "infisical")]
        assert!(registry.has("infisical"));
        #[cfg(feature = "aws")]
        assert!(registry.has("aws"));
        #[cfg(feature = "gcp")]
        assert!(registry.has("gcp"));
    }

    #[tokio::test]
    async fn init_failure_surfaces_at_resolution_time() {
        let mut registry = SecretRegistry::new();
        register_or_stub(&mut registry, "broken", || {
            Err("Failed to initialize broken resolver: boom".to_string())
        });

        assert!(registry.has("broken"));
        let spec = SecretSpec::new("anything");
        let err = registry
            .resolve("broken", "MY_SECRET", &spec)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MY_SECRET"));
        assert!(msg.contains("Failed to initialize broken resolver: boom"));
    }
}
