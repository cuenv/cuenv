//! Fallback behavior of the process-wide registry when no factory is
//! installed. Lives in its own integration-test binary because the global
//! registry is process-wide state; the install-first path is covered by
//! `global_install.rs`.

use cuenv_secrets::{SecretSpec, global_registry, install_registry_factory};

#[tokio::test]
async fn global_registry_falls_back_to_builtins() {
    let registry = global_registry();
    assert!(registry.has("env"));
    assert!(registry.has("exec"));
    assert!(!registry.has("onepassword"));

    temp_env::async_with_vars(
        [("GLOBAL_FALLBACK_SECRET", Some("fallback_value"))],
        async {
            let spec = SecretSpec::new("GLOBAL_FALLBACK_SECRET");
            let value = global_registry()
                .resolve("env", "GLOBAL_FALLBACK_SECRET", &spec)
                .await
                .expect("env resolution via fallback registry");
            assert_eq!(value, "fallback_value");
        },
    )
    .await;

    // The registry has been realized; installing a factory now must fail.
    let result = install_registry_factory(cuenv_secrets::SecretRegistry::with_builtins);
    assert!(matches!(
        result,
        Err(cuenv_secrets::SecretError::RegistryAlreadyInstalled)
    ));
}
