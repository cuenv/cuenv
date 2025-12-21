# Cuenv Codebase Cleanup Checklist

Based on Rust coding standards in `.github/agents/rust-coding-standards.agent.md`

## Summary

| Category | Count | Status |
|----------|-------|--------|
| Forbidden println!/eprintln! | 28 | ✅ |
| Production unwrap()/expect() | 60+ | 🔄 (~10 fixed) |
| Missing #[must_use] on error constructors | 10 | ✅ |
| Result<T, String> callbacks | 2 | ✅ |
| Imperative loops → functional | 35+ | ⬜ |

> Note: Many unwrap()/expect() calls listed are in test code (after `#[cfg(test)]`), which is acceptable.

---

## 1. Forbidden println!/eprintln! Usage ✅

**Standard**: "NEVER use println! or eprintln! - use cuenv_events macros"

### crates/ci/src/executor/orchestrator.rs ✅
- [x] All 17 instances replaced with cuenv_events macros and tracing

### crates/core/src/tasks/executor.rs ✅
- [x] All 2 instances replaced with `emit_stdout!` / `emit_stderr!`

### crates/cuenv/src/tui/rich.rs ✅
- [x] Both instances replaced with `tracing::warn!`

### crates/cuenv/src/main.rs ✅
- [x] 3 instances: panic hook and runtime init use scoped `#[allow]` (intentional - tracing may be unavailable)

### crates/cuenv/src/commands/task.rs ✅
- [x] All 3 instances replaced with `emit_stderr!`

### crates/cuenv/src/commands/mod.rs ✅
- [x] Instance replaced with `emit_ci_projects_discovered!`

---

## 2. Missing #[must_use] on Error Constructors ✅

**Standard**: "Constructor methods with #[must_use] for error creation"

### crates/core/src/lib.rs ✅
- [x] All 10 error constructors now have `#[must_use]`

---

## 3. Result<T, String> Callbacks (Use Proper Error Types) ✅

**Standard**: "Use thiserror for structured error enums"

- [x] `crates/core/src/tasks/discovery.rs:40` - `EvalFn` now returns `Result<Project, crate::Error>`
- [x] `crates/core/src/base/discovery.rs:26` - `BaseEvalFn` now returns `Result<Base, crate::Error>`
- [x] `DiscoveryError::EvalError` variants updated to use `#[source] crate::Error`

---

## 4. Production unwrap()/expect() Calls 🔄

**Standard**: "NO unwrap() or expect() in production code"

> Note: Items marked with 🧪 are in test code (`#[cfg(test)]`) and are acceptable.

### crates/core/src/paths.rs 🧪
All 11 calls are in test code - acceptable.

### crates/core/src/environment.rs 🧪
All 4 calls are in test code - acceptable.

### crates/core/src/tasks/executor.rs ✅
- [x] Line 252-253: Refactored to use `if let` pattern matching
- [x] Line 800-801: Replaced with `.ok_or_else(|| Error::execution(...))?`

### crates/core/src/lib.rs 🧪
All 5 calls are in test code - acceptable.

### crates/core/src/cache/tasks.rs ✅
- [x] Both instances replaced with `.map_err(...)?`

### crates/cuengine/src/lib.rs 🧪
All 6 calls are in test code (after line 611) - acceptable.

### crates/cuengine/src/validation.rs
- [ ] Line 105: `.expect("name is non-empty")` - needs review

### crates/cuenv/src/commands/info.rs
- [ ] Line 173, 190: `serde_json::to_string().unwrap()` - needs review

### crates/cuenv/src/commands/task.rs ✅
- [x] Line 318: Replaced with `.ok_or_else(|| Error::configuration(...))?`
- [x] Line 478: Replaced with `.ok_or_else(|| Error::execution(...))?`

### crates/cuenv/src/commands/task_list.rs
- [ ] Lines 337, 339, 355, 396, 400, 406, 408: `.expect("write to string")` - needs review

### crates/cuenv/src/commands/export.rs
- [ ] Lines 655-714: Multiple `.expect("write to string")` - needs review

### crates/cuenv/src/commands/mod.rs
- [ ] Line 740: `.expect("ModuleGuard invariant violated")` - internal invariant

### crates/cuenv/src/main.rs
- [ ] Line 954: `.expect("failed to get default state dir")` - needs review

### crates/cuenv/src/performance.rs
- [ ] Line 58: `.lock().expect("performance operations lock")` - mutex lock pattern

### crates/cuenv/src/providers.rs
- [ ] Line 98, 127: `.expect("LocalProvider should always be available")` - infallible operation

### crates/ci/src/executor/lock.rs
- [ ] Line 365: `.expect("System time is before UNIX epoch")` - system invariant

### crates/dagger/src/lib.rs (1 call)
- [ ] Line 119: `.expect("checked is_some above")`

---

## 5. Imperative Loops → Functional Patterns

**Standard**: "Prefer functional patterns - iterator chains over loops"

### Vec::new() + for loop + push() patterns

- [ ] `crates/buildkite/src/emitter.rs:59-73` - Steps collection → `.map().collect()`
- [ ] `crates/cuenv/src/completions.rs:177-189` - Completions → `.flat_map().collect()`
- [ ] `crates/cuenv/src/commands/task_list.rs:114` - Source groups → `.map().collect()`
- [ ] `crates/cuenv/src/commands/mod.rs:209` - IR tasks → `.flat_map().collect()`
- [ ] `crates/cuenv/src/commands/mod.rs:433` - IR tasks → `.flat_map().collect()`
- [ ] `crates/cuenv/src/commands/task.rs:647` - Task infos → `.map().collect()`
- [ ] `crates/cuenv/src/commands/sync.rs:44` - Files → `.map().collect()`
- [ ] `crates/cuenv/src/commands/sync.rs:70` - Files → `.map().collect()`
- [ ] `crates/cuenv/src/commands/env.rs:182` - Secrets → `.filter().map().collect()`
- [ ] `crates/cuenv/src/commands/hooks.rs:461` - Matching states → `.filter().collect()`
- [ ] `crates/core/src/tasks/executor.rs:314-315` - stdout/stderr lines → `.lines().collect()`
- [ ] `crates/core/src/tasks/graph.rs:76` - Nodes → `.enumerate().map().collect()`
- [ ] `crates/core/src/tasks/graph.rs:107` - Nodes → `.flat_map().collect()`
- [ ] `crates/core/src/tasks/discovery.rs:96` - Load failures → `.filter_map().collect()`
- [ ] `crates/ignore/src/lib.rs:165` - Lines → `.map().collect()`
- [ ] `crates/ignore/src/lib.rs:294` - Results → `.collect()`
- [ ] `crates/ci/src/discovery.rs:61` - Projects → `.filter_map().collect()`
- [ ] `crates/workspaces/src/detection.rs:79` - Detections → `.map().collect()`
- [ ] `crates/cubes/src/generator.rs:70` - Generated files → `.map().collect()`
- [ ] `crates/github/src/workflow/emitter.rs:387-410` - Steps → `.map().collect()`

### Nested for loops → flat_map()

- [ ] `crates/core/src/tasks/graph.rs:109-125` - Task nodes → `.flat_map()`
- [ ] `crates/core/src/tasks/graph.rs:157-166` - Dependency edges → `.flat_map()`
- [ ] `crates/cuenv/src/commands/task.rs:1878-2010` - Hook items → refactor with helpers
- [ ] `crates/cuenv/src/tui/state.rs:482-520` - Tree traversal → functional traversal
- [ ] `crates/workspaces/src/discovery/package_json.rs:65-75` - Members → `.flat_map()`
- [ ] `crates/workspaces/src/discovery/cargo_toml.rs:88-110` - Dependencies → `.flat_map()`

### Mutable counters → fold()

- [ ] `crates/core/src/hooks/approval.rs:507` - Summary parts → `.fold()`
- [ ] `crates/workspaces/src/resolver.rs:82-96` - Workspace deps → `.flat_map().collect()`
- [ ] `crates/core/src/tasks/io.rs:106-107` - File partitioning → `.partition()`
- [ ] `crates/secrets/src/resolvers/onepassword.rs:268-282` - References → `HashSet` or `.fold()`

### Filter-map-collect patterns

- [ ] `crates/ci/src/ir/validation.rs:58-105` - Validation errors → `.filter_map().collect()`
- [ ] `crates/core/src/environment.rs:367-378` - Env vars → `.filter().map().collect()`
- [ ] `crates/cuenv/src/commands/export.rs:406-430` - Hooks → `.flat_map().filter().collect()`
- [ ] `crates/workspaces/src/resolver.rs:101-110` - External deps → `.filter().map().collect()`

---

## Validation Commands

After completing each section, run:

```bash
cuenv task fmt.fix
cuenv task lint
cuenv task test.unit
```
