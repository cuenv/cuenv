//! Install semantics of the process-wide registry factory. Lives in its own
//! integration-test binary because the global registry is process-wide
//! state; the no-install fallback path is covered by `global_fallback.rs`.

use async_trait::async_trait;
use cuenv_secrets::{
    SecretError, SecretRegistry, SecretResolver, SecretSpec, global_registry,
    install_registry_factory,
};
use std::sync::Arc;

struct MarkerResolver;

#[async_trait]
impl SecretResolver for MarkerResolver {
    async fn resolve(&self, _name: &str, _spec: &SecretSpec) -> Result<String, SecretError> {
        Ok("marker".to_string())
    }

    fn provider_name(&self) -> &'static str {
        "marker"
    }
}

#[tokio::test]
async fn installed_factory_builds_the_global_registry() {
    install_registry_factory(|| {
        let mut registry = SecretRegistry::with_builtins();
        registry.register(Arc::new(MarkerResolver));
        registry
    })
    .expect("first install succeeds");

    // Installing twice fails, before or after realization.
    assert!(matches!(
        install_registry_factory(SecretRegistry::with_builtins),
        Err(SecretError::RegistryAlreadyInstalled)
    ));

    let registry = global_registry();
    assert!(registry.has("env"));
    assert!(registry.has("exec"));
    assert!(registry.has("marker"));

    let spec = SecretSpec::new("anything");
    let value = registry
        .resolve("marker", "anything", &spec)
        .await
        .expect("marker resolution");
    assert_eq!(value, "marker");

    assert!(matches!(
        install_registry_factory(SecretRegistry::with_builtins),
        Err(SecretError::RegistryAlreadyInstalled)
    ));
}
