//! Sync provider implementations.

mod ci;
mod codeowners;
mod cubes;
mod ignore;

pub use ci::CiSyncProvider;
pub use codeowners::CodeOwnersSyncProvider;
pub use cubes::CubesSyncProvider;
pub use ignore::IgnoreSyncProvider;

use super::registry::SyncRegistry;

/// Create the default registry with all built-in providers.
#[must_use]
pub fn default_registry() -> SyncRegistry {
    let mut registry = SyncRegistry::new();
    registry.register(IgnoreSyncProvider);
    registry.register(CodeOwnersSyncProvider);
    registry.register(CubesSyncProvider);
    registry.register(CiSyncProvider);
    registry
}
