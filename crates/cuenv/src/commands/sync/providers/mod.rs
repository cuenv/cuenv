//! Sync provider implementations.

mod ci;
mod cubes;
mod lock;
mod rules;

pub use ci::CiSyncProvider;
pub use cubes::CubesSyncProvider;
pub use lock::LockSyncProvider;
pub use rules::RulesSyncProvider;

use super::registry::SyncRegistry;

/// Create the default registry with all built-in providers.
#[must_use]
pub fn default_registry() -> SyncRegistry {
    let mut registry = SyncRegistry::new();
    registry.register(CubesSyncProvider);
    registry.register(CiSyncProvider);
    registry.register(RulesSyncProvider);
    registry.register(LockSyncProvider);
    registry
}
