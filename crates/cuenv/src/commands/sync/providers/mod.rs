//! Sync provider implementations.

mod ci;
mod codegen;
mod git_hooks;
mod lock;
mod rules;

pub use ci::CiSyncProvider;
pub use codegen::CodegenSyncProvider;
pub use git_hooks::GitHooksSyncProvider;
pub use lock::LockSyncProvider;
pub use rules::RulesSyncProvider;

use super::registry::SyncRegistry;

/// Create the default registry with all built-in providers.
#[must_use]
pub fn default_registry() -> SyncRegistry {
    let mut registry = SyncRegistry::new();
    registry.register(CodegenSyncProvider);
    registry.register(CiSyncProvider);
    registry.register(RulesSyncProvider);
    registry.register(LockSyncProvider);
    registry.register(GitHooksSyncProvider);
    registry
}
