# CI Module Refactoring Plan

This document tracks improvement opportunities identified during the review of the CI infrastructure (Phases 1-5, ~10,000 lines). The code is generally well-structured with good separation of concerns. Below are opportunities to improve long-term sustainability.

---

## 1. Trait-Based Abstractions for Better Composition

### 1.1 `CIProvider` trait coupling in `run_ci`
**File:** `crates/ci/src/executor/mod.rs` (lines 400-600)

The `run_ci` function is 200+ lines with tight coupling to `CIProvider`. Consider splitting:

```rust
// Proposed: Extract pipeline orchestration into trait
pub trait PipelineOrchestrator: Send + Sync {
    async fn execute(&self, ctx: ExecutionContext) -> Result<PipelineResult, ExecutorError>;
}

// Separate reporting from execution
pub trait ReportPublisher: Send + Sync {
    async fn publish(&self, report: &PipelineReport) -> Result<(), ReportError>;
}
```

### 1.2 Missing `SecretResolver` trait abstraction
**File:** `crates/ci/src/executor/secrets.rs`

Currently secrets are resolved directly from env vars. A trait would enable testing and alternative providers:

```rust
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, config: &SecretConfig) -> Result<String, SecretError>;
}

// Implementations: EnvSecretResolver, VaultResolver, OnePasswordResolver
```

---

## 2. Error Handling Improvements

### 2.1 Consider using `thiserror` `#[from]` more consistently
**Files:** Various error types

Some errors use manual `From` implementations while others use `#[from]`. Standardize for maintainability.

### 2.2 Add context to IO errors
**File:** `crates/ci/src/executor/cache.rs`

```rust
// Current (loses path context)
fs::write(&meta_path, meta_json)?;

// Better (preserves context)
fs::write(&meta_path, meta_json)
    .map_err(|e| CacheError::WriteError { path: meta_path.clone(), source: e })?;
```

### 2.3 `BackendError::Unavailable` should enable graceful degradation
The remote cache has `Unavailable` but callers often just propagate errors. Consider a `CacheResult<T>` that distinguishes "hard failures" from "cache unavailable, continue without".

---

## 3. Safety and Correctness

### 3.1 Unsafe environment variable access in tests
**File:** `crates/ci/src/executor/secrets.rs`

```rust
// Current (SAFETY comment isn't sufficient)
unsafe {
    std::env::set_var("TEST_SECRET_1", "value1");
}
```

Use `temp_env` crate or serial test execution instead of unsafe blocks.

### 3.2 `current_timestamp()` fallback silently returns 0
**File:** `crates/ci/src/executor/lock.rs`

```rust
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)  // Silent failure
}
```

Consider returning `Result` or using `expect` since this can't realistically fail on supported platforms.

### 3.3 `RemoteCacheBackend` connection lifecycle
**File:** `crates/ci/src/executor/remote.rs`

The `RwLock<Option<Channel>>` pattern could lead to stale connections. Consider connection pooling with health checks or using `tower`'s retry middleware.

---

## 4. Functional Design Patterns

### 4.1 Extract digest computation strategy
**File:** `crates/ci/src/compiler/digest.rs`

Digest computation has multiple concerns mixed. Consider:

```rust
pub trait DigestStrategy {
    fn compute(&self, task: &Task, context: &DigestContext) -> String;
}

// Implementations: ContentAddressedDigest, RuntimeAwareDigest
```

### 4.2 Builder pattern inconsistency
Some types use builder pattern (`CIExecutorConfig`), others don't (`GCConfig`). Standardize to builders for types with 3+ optional fields.

### 4.3 Pipeline execution could use state machine pattern
**File:** `crates/ci/src/executor/mod.rs`

The `execute_pipeline` method has implicit state transitions. A formal state machine would make the flow clearer:

```rust
enum PipelineState {
    Compiling,
    ResolvingSecrets,
    ComputingDigests,
    Executing(GroupIndex),
    Completed(PipelineResult),
    Failed(ExecutorError),
}
```

---

## 5. Loose Coupling Improvements

### 5.1 `CIExecutor` knows too much about secrets
The executor directly calls `secrets::resolve_all_task_secrets`. This should be injected:

```rust
pub struct CIExecutor<S: SecretResolver = EnvSecretResolver> {
    config: CIExecutorConfig,
    secret_resolver: S,
}
```

### 5.2 Hardcoded shell path
**File:** `crates/ci/src/executor/runner.rs`

```rust
let mut c = Command::new("/bin/sh");  // Hardcoded
```

Should be configurable or detected from environment.

### 5.3 Cache backends should be injected, not created
**File:** `crates/ci/src/executor/mod.rs`

Currently the executor creates cache backends internally. Injection would improve testability:

```rust
pub struct CIExecutor<C: CacheBackend = LocalCacheBackend> {
    config: CIExecutorConfig,
    cache: C,
}
```

---

## 6. Code Organization

### 6.1 `executor/mod.rs` is 800+ lines
Split into:
- `executor/orchestrator.rs` - Pipeline orchestration
- `executor/task_execution.rs` - Single task execution
- `executor/mod.rs` - Re-exports only

### 6.2 Consider feature flags for optional backends
The remote cache (Bazel RE) pulls in `tonic`, `prost`, etc. Make it optional:

```toml
[features]
default = ["local-cache"]
local-cache = []
remote-cache = ["tonic", "prost", "bazel-remote-apis"]
```

---

## 7. Documentation and Testing

### 7.1 Add integration test for cache fallback behavior
Verify that remote cache failures gracefully fall back to local cache.

### 7.2 Add property-based tests for digest computation
Digest determinism is critical. Use `proptest` to verify:
- Same inputs always produce same digest
- Different inputs produce different digests
- Order of inputs doesn't matter (or does, and is documented)

### 7.3 Missing doc comments on public items
Several public functions lack documentation (e.g., `compute_task_digest`, `policy_allows_read`).

---

## Priority Recommendations

### High Priority (correctness/safety)
- [x] Fix unsafe env var usage in tests (Phase 3.1 - completed)
- [x] Add context to IO errors (Phase 2.2 - completed in earlier commit)
- [x] Make remote cache failures non-fatal by default (Phase 2.3 - completed in earlier commit)
- [x] Fix `current_timestamp()` silent failure (Phase 3.2 - completed)

### Medium Priority (maintainability)
- [ ] Extract `SecretResolver` trait
- [x] Split `executor/mod.rs` (Phase 5.1 - completed, extracted orchestrator.rs)
- [ ] Add feature flags for remote cache

### Low Priority (nice-to-have)
- [ ] State machine for pipeline execution
- [ ] Standardize builder patterns
- [ ] Property-based tests for digests
