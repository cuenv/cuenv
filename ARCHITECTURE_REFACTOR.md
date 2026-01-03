# cuenv-core Architecture Refactor

Split cuenv-core (22,237 LOC) into 6 focused crates, reducing core to ~8K LOC.

## ⚠️ Breaking Changes Policy

**Backwards compatibility is explicitly NOT a goal.** This refactor prioritizes clean architecture over migration paths. All imports, re-exports, and public APIs will change without deprecation warnings or shims.

## Decisions

| Decision | Choice |
|----------|--------|
| Start phase | Phase 1: Hooks |
| Backwards compat | **No** - clean break, no re-exports, no deprecation shims |
| 1Password | Default on |
| Naming | `cuenv-task-*` |

---

## Phase 1: cuenv-hooks (4,100 LOC)

Extract background execution, state management, and approval system.

### Setup

- [x] Create `crates/hooks/Cargo.toml`
- [x] Create `crates/hooks/src/lib.rs`
- [x] Add `cuenv-hooks` to workspace `Cargo.toml` members

### Migration

- [x] Move `crates/core/src/hooks/executor.rs` → `crates/hooks/src/executor.rs`
- [x] Move `crates/core/src/hooks/state.rs` → `crates/hooks/src/state.rs`
- [x] Move `crates/core/src/hooks/approval.rs` → `crates/hooks/src/approval.rs`
- [x] Move `crates/core/src/hooks/types.rs` → `crates/hooks/src/types.rs`
- [x] Move tests from `crates/core/src/hooks/` to `crates/hooks/src/`

### Update Core

- [x] Remove `pub mod hooks;` from `crates/core/src/lib.rs`
- [x] Delete `crates/core/src/hooks/` directory
- [x] Remove `fs4`, `sysinfo` from `crates/core/Cargo.toml` (if unused elsewhere)

### Update Dependents

- [x] Run: `rg "cuenv_core::hooks" --type rust`
- [x] Update all imports: `cuenv_core::hooks::*` → `cuenv_hooks::*`
- [x] Add `cuenv-hooks` dependency where needed

### Validation

- [x] `cargo test -p cuenv-hooks`
- [x] `cargo test -p cuenv-core`
- [x] `cuenv task check`

### Update Documentation

- [x] Update `CLAUDE.md` if crate descriptions changed
- [x] Update `readme.md` references (no hooks references found)
- [x] Update any `docs/` files referencing moved modules

---

## Phase 2: cuenv-cache (885 LOC)

Extract content-addressed task caching infrastructure.

### Setup

- [ ] Create `crates/cache/Cargo.toml`
- [ ] Create `crates/cache/src/lib.rs`
- [ ] Add `cuenv-cache` to workspace `Cargo.toml` members

### Migration

- [ ] Move `crates/core/src/cache/tasks.rs` → `crates/cache/src/tasks.rs`
- [ ] Extract cache key logic to `crates/cache/src/keys.rs`
- [ ] Extract storage logic to `crates/cache/src/storage.rs`
- [ ] Move tests

### Update Core

- [ ] Remove `pub mod cache;` from `crates/core/src/lib.rs`
- [ ] Delete `crates/core/src/cache/` directory
- [ ] Remove `sha2`, `hex` from `crates/core/Cargo.toml` (if unused elsewhere)

### Update Dependents

- [ ] Run: `rg "cuenv_core::cache" --type rust`
- [ ] Update all imports: `cuenv_core::cache::*` → `cuenv_cache::*`
- [ ] Update `crates/core/src/tasks/executor.rs` to import from `cuenv_cache`

### Validation

- [ ] `cargo test -p cuenv-cache`
- [ ] `cargo test -p cuenv-core`
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 3: cuenv-task-graph (938 LOC)

Extract pure DAG algorithms (petgraph wrapper).

### Setup

- [ ] Create `crates/task-graph/Cargo.toml` (minimal deps: petgraph, thiserror)
- [ ] Create `crates/task-graph/src/lib.rs`
- [ ] Add `cuenv-task-graph` to workspace `Cargo.toml` members

### Migration

- [ ] Move `crates/core/src/tasks/graph.rs` → `crates/task-graph/src/graph.rs`
- [ ] Extract traversal logic to `crates/task-graph/src/traversal.rs`
- [ ] Extract validation logic to `crates/task-graph/src/validation.rs`
- [ ] Define `TaskNode` trait for generic task type
- [ ] Move tests (including `graph_advanced_tests.rs`)

### Update Core

- [ ] Update `crates/core/src/tasks/mod.rs` to import from `cuenv_task_graph`
- [ ] Delete `crates/core/src/tasks/graph.rs`
- [ ] Keep `petgraph` in core for now (may be used elsewhere)

### Update Dependents

- [ ] Run: `rg "cuenv_core::tasks::graph" --type rust`
- [ ] Update all imports

### Validation

- [ ] `cargo test -p cuenv-task-graph`
- [ ] `cargo test -p cuenv-core`
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 4: cuenv-task-discovery (908 LOC)

Extract workspace scanning and TaskRef resolution.

### Setup

- [ ] Create `crates/task-discovery/Cargo.toml`
- [ ] Create `crates/task-discovery/src/lib.rs`
- [ ] Add `cuenv-task-discovery` to workspace `Cargo.toml` members

### Migration

- [ ] Move `crates/core/src/tasks/discovery.rs` → `crates/task-discovery/src/discovery.rs`
- [ ] Extract TaskRef parsing to `crates/task-discovery/src/task_ref.rs`
- [ ] Extract workspace logic to `crates/task-discovery/src/workspace.rs`
- [ ] Move tests

### Update Core

- [ ] Update `crates/core/src/tasks/mod.rs` to import from `cuenv_task_discovery`
- [ ] Delete `crates/core/src/tasks/discovery.rs`
- [ ] Remove `ignore`, `walkdir`, `globset` from `crates/core/Cargo.toml` (if unused)

### Update Dependents

- [ ] Run: `rg "cuenv_core::tasks::discovery" --type rust`
- [ ] Update all imports

### Validation

- [ ] `cargo test -p cuenv-task-discovery`
- [ ] `cargo test -p cuenv-core`
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 5: cuenv-task-index (782 LOC)

Extract task indexing and path normalization.

### Setup

- [ ] Create `crates/task-index/Cargo.toml` (minimal deps: thiserror)
- [ ] Create `crates/task-index/src/lib.rs`
- [ ] Add `cuenv-task-index` to workspace `Cargo.toml` members

### Migration

- [ ] Move `crates/core/src/tasks/index.rs` → `crates/task-index/src/index.rs`
- [ ] Extract normalization to `crates/task-index/src/normalization.rs`
- [ ] Move tests

### Update Core

- [ ] Update `crates/core/src/tasks/mod.rs` to import from `cuenv_task_index`
- [ ] Delete `crates/core/src/tasks/index.rs`

### Update Dependents

- [ ] Run: `rg "cuenv_core::tasks::index" --type rust`
- [ ] Update all imports

### Validation

- [ ] `cargo test -p cuenv-task-index`
- [ ] `cargo test -p cuenv-core`
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 6: Secrets Refactoring

Remove 1Password hardcoding, use trait-based registry.

### Update cuenv-secrets

- [ ] Add `SecretRegistry` struct to `crates/secrets/src/lib.rs`
- [ ] Add `register()` and `resolve()` methods
- [ ] Update `SecretResolver` trait with `resolver_name()` method

### Update Core

- [ ] Remove `pub use cuenv_1password::*` from `crates/core/src/secrets/mod.rs`
- [ ] Add `create_default_registry()` function
- [ ] Use feature flag for 1Password: `#[cfg(feature = "1password")]`

### Update Cargo.toml

- [ ] Add to `crates/core/Cargo.toml`:
  ```toml
  [features]
  default = ["1password"]
  1password = ["dep:cuenv-1password"]

  [dependencies]
  cuenv-1password = { workspace = true, optional = true }
  ```

### Update Dependents

- [ ] Run: `rg "cuenv_core::secrets::OnePassword" --type rust`
- [ ] Update to use registry pattern

### Validation

- [ ] `cargo test -p cuenv-secrets`
- [ ] `cargo test -p cuenv-core`
- [ ] `cargo test -p cuenv-core --no-default-features` (verify 1password is optional)
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 7: CI Consolidation

Merge `core/ci.rs` (618 LOC) into existing cuenv-ci crate.

### Migration

- [ ] Create `crates/ci/src/detection.rs`
- [ ] Move CI provider detection from `crates/core/src/ci.rs`
- [ ] Move `CIProvider` enum and related types
- [ ] Update `crates/ci/src/lib.rs` to export detection module
- [ ] Move tests

### Update Core

- [ ] Remove `pub mod ci;` from `crates/core/src/lib.rs`
- [ ] Delete `crates/core/src/ci.rs`
- [ ] Add `cuenv-ci` dependency to `crates/core/Cargo.toml`

### Update Dependents

- [ ] Run: `rg "cuenv_core::ci" --type rust`
- [ ] Update all imports: `cuenv_core::ci::*` → `cuenv_ci::*`

### Validation

- [ ] `cargo test -p cuenv-ci`
- [ ] `cargo test -p cuenv-core`
- [ ] `cuenv task check`

### Update Documentation

- [ ] Update `CLAUDE.md` if crate descriptions changed
- [ ] Update `readme.md` references
- [ ] Update any `docs/` files referencing moved modules

---

## Phase 8: cuenv-cubes → cuenv-codegen Rename

Rename cuenv-cubes crate to cuenv-codegen for clarity.

### Directory Rename

- [ ] Rename `crates/cubes/` → `crates/codegen/`
- [ ] Update `crates/codegen/Cargo.toml`: `name = "cuenv-codegen"`

### Workspace Config

- [ ] Update root `Cargo.toml`: workspace member path and dependency
- [ ] Run `cargo update` to regenerate lock file

### Update Imports

- [ ] `crates/cuenv/Cargo.toml`: dependency name
- [ ] `crates/cuenv/src/providers/cubes.rs` → `codegen.rs`
- [ ] `crates/cuenv/src/providers/mod.rs`: module and re-export
- [ ] `crates/cuenv/src/builder.rs`: import and usage
- [ ] `crates/cuenv/src/commands/sync/providers/cubes.rs` → `codegen.rs`
- [ ] `crates/cuenv/src/commands/sync/functions.rs`: function names

### Update Manifest Types

- [ ] `crates/core/src/manifest/mod.rs`: `cube` field → `codegen` field
- [ ] Update `CubeConfig` → `CodegenConfig` if applicable

### Update Schema

- [ ] Rename `schema/cubes.cue` → `schema/codegen.cue`
- [ ] Rename `schema/cubes/` directory → `schema/codegen/`
- [ ] Update import paths in schema files

### Update Examples

- [ ] `examples/cube-hello/env.cue`: update imports and references

### Update Documentation

- [ ] `CLAUDE.md`: crate table
- [ ] `readme.md`: command examples
- [ ] `crates/cubes/README.md` → `crates/codegen/README.md`
- [ ] `docs/src/content/docs/explanation/cuenv-cubes.md` → `cuenv-codegen.md`
- [ ] `docs/src/content/docs/how-to/cubes.md` → `codegen.md`
- [ ] Update docs index files

### Validation

- [ ] `cargo test -p cuenv-codegen`
- [ ] `cargo test -p cuenv`
- [ ] `cuenv task check`

---

## Final Validation

After all phases complete:

- [ ] `cuenv fmt --fix`
- [ ] `cuenv task lint`
- [ ] `cuenv task test.unit`
- [ ] `cuenv task test.bdd`
- [ ] `cuenv exec -- cargo run -- version`
- [ ] `cuenv exec -- cargo run -- env print --path examples/env-basic --package examples`
- [ ] Verify core LOC is ~8K (down from 22K)
- [ ] Update CHANGELOG.md with breaking changes
