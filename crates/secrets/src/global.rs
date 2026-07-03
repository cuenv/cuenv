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
/// already installed — explicitly, or implicitly because a resolution
/// already ran and locked in the built-in fallback. The factory `OnceLock`
/// is the single decision point, so a success here means the installed
/// factory is the one the registry will use (or already used).
pub fn install_registry_factory(
    factory: impl Fn() -> SecretRegistry + Send + Sync + 'static,
) -> Result<(), SecretError> {
    FACTORY
        .set(Box::new(factory))
        .map_err(|_| SecretError::RegistryAlreadyInstalled)
}

/// Get the process-wide secret registry, realizing it on first use.
///
/// Uses the installed factory when present; otherwise locks in the
/// dependency-free built-ins (`env` + `exec`) as the factory, so a later
/// [`install_registry_factory`] reliably fails instead of silently being
/// ignored.
pub fn global_registry() -> &'static SecretRegistry {
    REGISTRY.get_or_init(|| {
        let factory =
            FACTORY.get_or_init(|| Box::new(SecretRegistry::with_builtins) as RegistryFactory);
        factory()
    })
}
