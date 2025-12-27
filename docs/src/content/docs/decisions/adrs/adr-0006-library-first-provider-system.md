---
id: ADR-0006
title: Library-First Architecture with Unified Provider System
status: Accepted
decision_date: 2025-12-27
approvers:
  - Core Maintainers
related_features: []
supersedes: []
superseded_by: []
---

## Context

The cuenv CLI currently has a tightly coupled architecture:

1. **Hardcoded Providers**: Sync providers (ci, cubes, rules) are registered via a static `default_registry()` factory function
2. **Binary-Only**: The `cuenv` crate is only consumable as a binary, not as a library
3. **Hardcoded CLI**: The `SyncCommands` enum in `cli.rs` must be modified for each new provider
4. **Single-Capability Providers**: Each provider type (sync, runtime, secret) has a separate registration mechanism

This prevents:
- External crates from extending cuenv without forking
- Building custom cuenv distributions with different provider sets
- Providers that offer multiple capabilities (e.g., Dagger providing both sync and runtime)

## Decision

### 1. Library-First Architecture

The `cuenv` crate will be refactored to expose both library and binary targets:

```toml
[lib]
name = "cuenv"
path = "src/lib.rs"

[[bin]]
name = "cuenv"
path = "src/main.rs"
```

The binary becomes a thin wrapper:
```rust
fn main() -> cuenv::Result<()> {
    cuenv::Cuenv::builder()
        .with_defaults()
        .build()
        .run()
}
```

### 2. Unified Provider System

A provider is a unit that implements one or more capability traits:

```rust
/// Base trait for all providers
pub trait Provider: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;  // For capability detection
}

/// Capability: Sync files from CUE configuration
#[async_trait]
pub trait SyncCapability: Provider {
    fn build_sync_command(&self) -> clap::Command;
    async fn sync_path(&self, path: &Path, options: &SyncOptions) -> Result<SyncResult>;
    async fn sync_workspace(&self, options: &SyncOptions) -> Result<SyncResult>;
}

/// Capability: Execute tasks
#[async_trait]
pub trait RuntimeCapability: Provider {
    async fn execute(&self, task: &Task) -> Result<Output>;
}

/// Capability: Resolve secrets
#[async_trait]
pub trait SecretCapability: Provider {
    async fn resolve(&self, reference: &str) -> Result<Secret>;
}
```

### 3. Unified Registration via Builder Pattern

A single `with_provider()` method registers providers:

```rust
Cuenv::builder()
    .with_provider(CiProvider)           // SyncCapability
    .with_provider(DaggerProvider::new()) // SyncCapability + RuntimeCapability
    .with_provider(VaultProvider::new())  // SecretCapability
    .build()
    .run()
```

The `ProviderRegistry` filters by capability:
```rust
impl ProviderRegistry {
    pub fn sync_providers(&self) -> impl Iterator<Item = &dyn SyncCapability>;
    pub fn runtime_providers(&self) -> impl Iterator<Item = &dyn RuntimeCapability>;
    pub fn secret_providers(&self) -> impl Iterator<Item = &dyn SecretCapability>;
}
```

### 4. Dynamic CLI Generation

CLI subcommands are generated from registered provider capabilities:

```rust
impl Cuenv {
    fn build_cli(&self) -> clap::Command {
        let mut sync_cmd = Command::new("sync");
        for provider in self.registry.sync_providers() {
            sync_cmd = sync_cmd.subcommand(provider.build_sync_command());
        }
        // ... other capability-based commands
    }
}
```

## Consequences

### Positive

1. **Extensibility**: External crates can add providers without forking
2. **Multi-Capability Providers**: A single provider (e.g., Dagger) can offer sync, runtime, and secret capabilities
3. **Custom Distributions**: Organizations can build cuenv variants with specific provider sets
4. **Cleaner Architecture**: Removes hardcoded CLI enums and static registries
5. **Testability**: Providers can be mocked individually

### Negative

1. **Breaking Internal Change**: Provider registration mechanism changes entirely
2. **Runtime Dispatch**: Capability detection uses `as_any()` downcast (minor perf cost)
3. **Migration Effort**: Existing providers must be refactored to implement base `Provider` trait

### Neutral

1. **No CLI Breaking Changes**: `cuenv sync ci` continues to work unchanged
2. **Incremental Capability Addition**: New capability traits can be added without modifying existing providers

## Alternatives Considered

### 1. Separate Builder Methods Per Capability

```rust
.with_sync_provider(CiProvider)
.with_runtime_provider(DaggerRuntime)
.with_secret_provider(VaultProvider)
```

**Rejected**: Prevents multi-capability providers and requires separate registration for each capability a provider supports.

### 2. Plugin System with Dynamic Loading

Load providers from shared libraries at runtime.

**Rejected**: Adds complexity (ABI stability, FFI), harder to debug, security concerns. Can be added later if needed.

### 3. Inventory/Linkme for Compile-Time Discovery

```rust
#[inventory::submit]
static _: &dyn Provider = &CiProvider;
```

**Rejected**: Magic, harder to understand, doesn't allow custom provider sets per binary.

## Related Documents

- [crates/cuenv/src/commands/sync/provider.rs](crates/cuenv/src/commands/sync/provider.rs) - Current SyncProvider trait
- [crates/cuenv/src/commands/sync/registry.rs](crates/cuenv/src/commands/sync/registry.rs) - Current SyncRegistry
- [crates/cuenv/src/commands/sync/providers/mod.rs](crates/cuenv/src/commands/sync/providers/mod.rs) - Current `default_registry()`
- [crates/cuenv/src/cli.rs](crates/cuenv/src/cli.rs) - Current hardcoded CLI

## Status

Accepted — implementation in progress on feature branch.

## Implementation Order

1. Add library target to Cargo.toml
2. Create `Provider` base trait and capability traits
3. Create `ProviderRegistry` with capability filtering
4. Create `CuenvBuilder` with `with_provider()`
5. Refactor existing providers (ci, cubes, rules)
6. Dynamic CLI generation
7. Simplify main.rs
