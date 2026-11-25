//! Sync provider implementations.

mod ci;
mod codegen;
mod git_hooks;
pub mod infrastructure;
mod lock;
pub mod rules;
mod scripts;
mod vcs;

use ci::CiSyncProvider;
use codegen::CodegenSyncProvider;
use git_hooks::GitHooksSyncProvider;
use infrastructure::InfrastructureSyncProvider;
use lock::LockSyncProvider;
use rules::RulesSyncProvider;
use scripts::ScriptsSyncProvider;
use vcs::VcsSyncProvider;

use super::registry::SyncRegistry;

/// Create the default registry with all built-in providers.
#[must_use]
pub fn default_registry() -> SyncRegistry {
    let mut registry = SyncRegistry::new();
    // First: it rewrites a file the other providers evaluate.
    registry.register(ScriptsSyncProvider);
    registry.register(CodegenSyncProvider);
    registry.register(CiSyncProvider);
    registry.register(RulesSyncProvider);
    registry.register(VcsSyncProvider);
    registry.register(LockSyncProvider);
    registry.register(InfrastructureSyncProvider);
    registry.register(GitHooksSyncProvider);
    registry
}
