//! Process-wide secret registry.
//!
//! The application composition root (the cuenv CLI) installs a registry
//! *factory* at startup via [`install_registry_factory`]; the registry itself
//! is realized lazily on the first [`global_registry`] call. This keeps
//! expensive provider initialization (e.g. the 1Password WASM runtime) off
//! the hot path of invocations that never resolve a secret, while letting
//! library consumers resolve `env`/`exec` secrets with no setup at all: when
//! no factory is installed, the registry falls back to
//! [`SecretRegistry::with_builtins`].

use crate::{SecretError, SecretRegistry};
use std::sync::OnceLock;

type RegistryFactory = Box<dyn Fn() -> SecretRegistry + Send + Sync>;

static FACTORY: OnceLock<RegistryFactory> = OnceLock::new();
static REGISTRY: OnceLock<SecretRegistry> = OnceLock::new();

/// Install the factory that builds the process-wide secret registry.
///
/// Call once from the application composition root, before any secret
/// resolution. The factory runs at most once, on the first
/// [`global_registry`] call.
///
/// # Errors
///
/// Returns [`SecretError::RegistryAlreadyInstalled`] if a factory was
/// already installed or the registry has already been realized (e.g. a
/// resolution already ran against the built-in fallback). Installation is
/// not synchronized against concurrent first resolution — install before
/// spawning work that resolves secrets.
pub fn install_registry_factory(
    factory: impl Fn() -> SecretRegistry + Send + Sync + 'static,
) -> Result<(), SecretError> {
    if REGISTRY.get().is_some() {
        return Err(SecretError::RegistryAlreadyInstalled);
    }
    FACTORY
        .set(Box::new(factory))
        .map_err(|_| SecretError::RegistryAlreadyInstalled)
}

/// Get the process-wide secret registry, realizing it on first use.
///
/// Uses the installed factory when present, otherwise falls back to the
/// dependency-free built-ins (`env` + `exec`).
pub fn global_registry() -> &'static SecretRegistry {
    REGISTRY.get_or_init(|| {
        FACTORY
            .get()
            .map_or_else(SecretRegistry::with_builtins, |factory| factory())
    })
}
