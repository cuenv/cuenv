//! Sync provider implementations.

mod ci;
mod codegen;
mod git_hooks;
mod infrastructure;
mod lock;
mod rules;
mod vcs;

use ci::CiSyncProvider;
use codegen::CodegenSyncProvider;
use git_hooks::GitHooksSyncProvider;
use infrastructure::InfrastructureSyncProvider;
use lock::LockSyncProvider;
use rules::RulesSyncProvider;
use vcs::VcsSyncProvider;

use super::registry::SyncRegistry;

/// Create the default registry with all built-in providers.
#[must_use]
pub fn default_registry() -> SyncRegistry {
    let mut registry = SyncRegistry::new();
    // Infrastructure first: it writes the generated provider types in
    // `cue.mod/gen` from `cuenv.lock` without evaluating anything, and every
    // later provider evaluates (and caches) projects that import them.
    registry.register(InfrastructureSyncProvider);
    registry.register(CodegenSyncProvider);
    registry.register(CiSyncProvider);
    registry.register(RulesSyncProvider);
    registry.register(VcsSyncProvider);
    registry.register(LockSyncProvider);
    registry.register(GitHooksSyncProvider);
    registry
}
