# cuenv-core Architecture Refactor

Split cuenv-core (22,237 LOC) into 6 focused crates, reducing core to ~8K LOC.

**Progress:** 60% complete (98/163 tasks)
- Phases 1-4: Complete
- Phase 5: Deferred (task index extraction - complex coupling)
- Phase 6: Complete (secrets registry pattern)
- Phase 7: Deferred (CI consolidation - circular dependency)
- Phase 8: Deferred (cuenv-cubes rename - breaking change)

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

- [x] Create `crates/cache/Cargo.toml`
- [x] Create `crates/cache/src/lib.rs`
- [x] Add `cuenv-cache` to workspace `Cargo.toml` members

### Migration

- [x] Move `crates/core/src/cache/tasks.rs` → `crates/cache/src/tasks.rs`
- [x] Extract cache key logic to `crates/cache/src/keys.rs` (kept in tasks.rs - simpler)
- [x] Extract storage logic to `crates/cache/src/storage.rs` (kept in tasks.rs - simpler)
- [x] Move tests
- [x] Move `snapshot_workspace_tar_zst` from `core/tasks/io.rs` to cache crate
- [x] Create `crates/cache/src/error.rs` for cache-specific errors

### Update Core

- [x] Remove `pub mod cache;` from `crates/core/src/lib.rs`
- [x] Delete `crates/core/src/cache/` directory
- [x] Remove `tar`, `zstd` from `crates/core/Cargo.toml` (moved to cache crate)
- [x] Keep `sha2`, `hex` in core (still used by tasks/io.rs)

### Update Dependents

- [x] Run: `rg "cuenv_core::cache" --type rust`
- [x] Update all imports: `cuenv_core::cache::*` → `cuenv_cache::*`
- [x] Update `crates/cuenv/src/commands/task_list.rs` to import from `cuenv_cache`

### Validation

- [x] `cargo test -p cuenv-cache`
- [x] `cargo test -p cuenv-core`
- [x] `cuenv task check`

### Update Documentation

- [x] Update `CLAUDE.md` if crate descriptions changed
- [x] Update `readme.md` references (no cache references found)
- [x] Update any `docs/` files referencing moved modules (none found)

---

## Phase 3: cuenv-task-graph (938 LOC)

Extract pure DAG algorithms (petgraph wrapper).

**Implementation Note**: Rather than moving graph.rs wholesale (which would require cuenv-task-graph to depend on cuenv-core's Task type, creating circular dependencies), we:
1. Created a standalone cuenv-task-graph crate with a generic `TaskNodeData` trait
2. Updated cuenv-core's graph.rs to wrap the new library
3. Implemented `TaskNodeData` for `Task` in cuenv-core

### Setup

- [x] Create `crates/task-graph/Cargo.toml` (minimal deps: petgraph, thiserror, tracing)
- [x] Create `crates/task-graph/src/lib.rs`
- [x] Add `cuenv-task-graph` to workspace `Cargo.toml` members

### Migration

- [x] Create generic `TaskGraph<T>` in `crates/task-graph/src/graph.rs`
- [x] Extract traversal types to `crates/task-graph/src/traversal.rs`
- [x] Extract validation logic to `crates/task-graph/src/validation.rs`
- [x] Define `TaskNodeData` trait for generic task type
- [x] Add standalone tests in cuenv-task-graph crate

### Update Core

- [x] Update `crates/core/src/tasks/graph.rs` to wrap `cuenv_task_graph::TaskGraph<Task>`
- [x] Implement `TaskNodeData` for `Task` in core
- [x] Add `cuenv-task-graph` dependency to `crates/core/Cargo.toml`
- [x] Keep `petgraph` in core for now (used for NodeIndex type)
- [x] Keep `graph_advanced_tests.rs` in core (tests core-specific group building logic)

### Update Dependents

- [x] No external imports of `cuenv_core::tasks::graph::TaskGraph` needed updating
- [x] `TaskNode` type alias maintained for API compatibility

### Validation

- [x] `cargo test -p cuenv-task-graph` (14 tests pass)
- [x] `cargo test -p cuenv-core tasks::graph` (52 tests pass)
- [x] `cargo check --workspace` (all crates compile)

### Update Documentation

- [x] Update `CLAUDE.md` if crate descriptions changed
- [x] Update `readme.md` references (no references to task graph found)
- [x] Update any `docs/` files referencing moved modules (none found)

---

## Phase 4: cuenv-task-discovery (908 LOC)

Extract workspace scanning and TaskRef resolution.

**Implementation Note**: The discovery module depends on core types (Task, Project, TaskIndex, TaskMatcher, ArgMatcher, TaskRef). Rather than creating circular dependencies:
1. `cuenv-task-discovery` depends on `cuenv-core` for these types
2. Consumers (cuenv binary) import directly from `cuenv-task-discovery`
3. Core does NOT depend on task-discovery (no re-exports, avoiding cycles)

### Setup

- [x] Create `crates/task-discovery/Cargo.toml`
- [x] Create `crates/task-discovery/src/lib.rs`
- [x] Add `cuenv-task-discovery` to workspace `Cargo.toml` members

### Migration

- [x] Move `crates/core/src/tasks/discovery.rs` → `crates/task-discovery/src/lib.rs`
- [x] Extract TaskRef parsing to `crates/task-discovery/src/task_ref.rs` (kept in lib.rs - TaskRef stays in manifest)
- [x] Extract workspace logic to `crates/task-discovery/src/workspace.rs` (kept in lib.rs - simpler)
- [x] Move tests

### Update Core

- [x] Remove `pub mod discovery;` from `crates/core/src/tasks/mod.rs`
- [x] Delete `crates/core/src/tasks/discovery.rs`
- [x] Remove `regex` from `crates/core/Cargo.toml` (moved to task-discovery)
- [x] Keep `ignore`, `walkdir`, `globset` in core (still used by base/discovery.rs and rules/discovery.rs)

### Update Dependents

- [x] Run: `rg "cuenv_core::tasks::discovery" --type rust`
- [x] Update all imports in cuenv binary to use `cuenv_task_discovery`

### Validation

- [x] `cargo test -p cuenv-task-discovery` (29 tests pass)
- [x] `cargo test -p cuenv-core` (444 passed, 1 flaky env var test - pre-existing)
- [x] `cuenv task check` (2755 tests, 1 flaky)

### Update Documentation

- [x] Update `CLAUDE.md` if crate descriptions changed
- [x] Update `readme.md` references (no discovery references found)
- [x] Update any `docs/` files referencing moved modules (none found)

---

## Phase 5: cuenv-task-index (782 LOC) - DEFERRED

Extract task indexing and path normalization.

**Decision: DEFERRED** - The TaskIndex module is tightly coupled to core's Task, TaskDefinition, TaskGroup, ParallelGroup, and Tasks types. Extracting it would require either:
1. Moving core task types to a shared types crate first
2. Using traits/generics to abstract over task types

The effort doesn't justify the benefit for ~400 LOC of actual code. This module can be extracted later if core's task types are factored out.

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

**Implementation Note**: Added `SecretRegistry` to `cuenv-secrets` with dynamic resolver registration. The `SecretResolver` trait already had `provider_name()` method. Updated `cuenv-core` to:
1. Add `create_default_registry()` function that registers env, exec, and 1password resolvers
2. Use feature flag for 1Password (`1password` feature, default on)
3. Updated `Secret::resolve()` to use registry pattern with `resolve_with_registry()` method

### Update cuenv-secrets

- [x] Add `SecretRegistry` struct to `crates/secrets/src/registry.rs`
- [x] Add `register()` and `resolve()` methods
- [x] `SecretResolver` trait already has `provider_name()` method

### Update Core

- [x] Remove `pub use cuenv_1password::*` from `crates/core/src/secrets/mod.rs` (now conditional)
- [x] Add `create_default_registry()` function
- [x] Use feature flag for 1Password: `#[cfg(feature = "1password")]`

### Update Cargo.toml

- [x] Add to `crates/core/Cargo.toml`:
  ```toml
  [features]
  default = ["1password"]
  1password = ["dep:cuenv-1password"]

  [dependencies]
  cuenv-1password = { workspace = true, optional = true }
  ```

### Update Dependents

- [x] Run: `rg "cuenv_core::secrets::OnePassword" --type rust` (no external usages found)
- [x] Update to use registry pattern

### Validation

- [x] `cargo test -p cuenv-secrets` (81 tests pass)
- [x] `cargo test -p cuenv-core` (445 tests pass)
- [x] `cargo check -p cuenv-core --no-default-features` (verify 1password is optional)
- [x] `cuenv task check` (2765 tests pass, 1 pre-existing flaky)

### Update Documentation

- [x] Update `CLAUDE.md` if crate descriptions changed (no changes needed)
- [x] Update `readme.md` references (no references found)
- [x] Update any `docs/` files referencing moved modules (none found)

---

## Phase 7: CI Consolidation - DEFERRED

Merge `core/ci.rs` (618 LOC) into existing cuenv-ci crate.

**Decision: DEFERRED** - The CI types in `core/ci.rs` are used by multiple crates:
- `cuenv-ci` (executor, compiler, emitter, pipeline)
- `cuenv-github` (workflow emitter, config)
- `cuenv-buildkite` (emitter)
- `cuenv` binary (CI commands)

Moving these types to `cuenv-ci` would create a circular dependency since `cuenv-ci` already depends on `cuenv-core`. Options:
1. Create a new `cuenv-ci-types` crate (similar to how `cuenv-task-graph` was extracted)
2. Keep types in core and move execution logic only

The effort doesn't justify the benefit for ~618 LOC. This module can be extracted later if we create a shared types crate.

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

## Phase 8: cuenv-cubes → cuenv-codegen Rename - DEFERRED

Rename cuenv-cubes crate to cuenv-codegen for clarity.

**Decision: DEFERRED** - This rename is a breaking change that affects:
1. CLI command: `cuenv sync cubes` → `cuenv sync codegen`
2. CUE manifest field: `cube` → `codegen`
3. CUE schema directory: `schema/cubes/` → `schema/codegen/`
4. All user documentation and examples

The rename should be coordinated with a major version bump or deprecation cycle.
Consider adding an alias for backwards compatibility if implemented.

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
