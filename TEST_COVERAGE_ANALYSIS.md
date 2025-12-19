# Test Coverage Analysis

## Executive Summary

The cuenv codebase has **strong test coverage in core components** (CLI, FFI bridge, task execution, workspace parsing) but has **significant gaps** in CI integration, event rendering, VCS providers, and the Dagger backend.

## Coverage by Crate

### Excellent Coverage

| Crate | Integration Tests | Unit Tests | Notes |
|-------|-------------------|------------|-------|
| **cuenv** (CLI) | 10 files | 60+ | BDD tests, E2E workflows |
| **cuengine** (FFI) | 7 files | 30+ | Property-based tests, concurrency tests |
| **workspaces** | 3 files + fixtures | 60+ | Real workspace config fixtures |
| **core** | 2 files | 79+ in tasks/ | Strong task graph testing |

### Moderate Coverage

| Crate | Integration Tests | Unit Tests | Notes |
|-------|-------------------|------------|-------|
| **cubes** | 1 file | 20+ | Code generation logic tested |
| **release** | 0 | 40+ inline | No E2E version bump testing |
| **events** | 0 | 20+ inline | Core event types tested, renderers not |

### Minimal/No Coverage

| Crate | Integration Tests | Unit Tests | Gaps |
|-------|-------------------|------------|------|
| **ci** | 0 | 15+ | Only markdown report has tests |
| **github** | 0 | 4 | Only parsing helpers tested |
| **gitlab** | 0 | 5 | Only CODEOWNERS parsing |
| **bitbucket** | 0 | 5 | Only CODEOWNERS parsing |
| **dagger** | 0 | **0** | **296 lines with ZERO tests** |
| **codeowners** | 0 | 10+ | Basic parsing only |
| **ignore** | 0 | 5+ | Minimal testing |

---

## Priority Improvement Areas

### 1. **Dagger Backend - CRITICAL** (crates/dagger)

**Current State:** 296 lines of production code with zero tests.

**Risk:** The Dagger backend is a core execution pathway that:
- Executes tasks in containers
- Manages container chaining (`from` references)
- Handles secret mounting
- Manages cache volumes

**Recommended Tests:**
```rust
// Unit tests for DaggerBackend
#[cfg(test)]
mod tests {
    // Test configuration validation
    #[test]
    fn test_requires_image_or_from() { }

    #[test]
    fn test_requires_command() { }

    #[test]
    fn test_from_requires_cached_container() { }

    // Test container cache operations
    #[test]
    fn test_container_cache_insert_and_lookup() { }

    // Test create_dagger_backend factory
    #[test]
    fn test_create_dagger_backend_with_config() { }

    #[test]
    fn test_create_dagger_backend_no_config() { }
}

// Integration tests (require Dagger daemon)
#[tokio::test]
#[ignore] // Requires dagger daemon
async fn test_simple_container_execution() { }

#[tokio::test]
#[ignore]
async fn test_container_chaining() { }

#[tokio::test]
#[ignore]
async fn test_secret_mounting() { }
```

### 2. **CI Executor & Affected Task Detection** (crates/ci)

**Current State:**
- `executor.rs` (282 lines) - **0 tests**
- `affected.rs` (209 lines) - **0 tests**
- `discovery.rs` - **0 tests**

**Risk:** The CI affected-task algorithm determines which tasks run in CI. Bugs here cause either:
- False negatives (missed tasks) → silent failures in production
- False positives (unnecessary tasks) → wasted CI time/cost

**Recommended Tests:**
```rust
// crates/ci/tests/affected_tests.rs
#[test]
fn test_simple_file_matches_input_glob() { }

#[test]
fn test_glob_wildcard_matching() { }

#[test]
fn test_simple_path_prefix_matching() { }

#[test]
fn test_transitive_dependency_affected() { }

#[test]
fn test_external_dependency_cross_project() { }

#[test]
fn test_unrelated_changes_no_affected_tasks() { }

#[test]
fn test_release_event_runs_all_tasks() { }

// crates/ci/tests/executor_tests.rs
#[tokio::test]
async fn test_dry_run_no_execution() { }

#[tokio::test]
async fn test_missing_pipeline_error() { }

#[tokio::test]
async fn test_no_projects_error() { }

#[tokio::test]
async fn test_task_failure_propagates() { }

#[tokio::test]
async fn test_report_generation() { }
```

### 3. **VCS Provider Integration** (crates/github, gitlab, bitbucket)

**Current State:**
- Only trivial helper functions tested (repo parsing, PR number extraction)
- No mocked API integration tests
- No changed file detection tests

**Recommended Tests:**
```rust
// Using mockito or wiremock for API mocking
#[tokio::test]
async fn test_create_check_run() { }

#[tokio::test]
async fn test_complete_check_run_success() { }

#[tokio::test]
async fn test_complete_check_run_failure() { }

#[tokio::test]
async fn test_upload_report_posts_pr_comment() { }

#[tokio::test]
async fn test_changed_files_pr_event() { }

#[tokio::test]
async fn test_changed_files_push_event() { }

#[tokio::test]
async fn test_changed_files_shallow_clone_fallback() { }
```

### 4. **Event Renderers** (crates/events/src/renderers)

**Current State:**
- `cli.rs` (268 lines) - **0 tests**
- `json.rs` - **0 tests**

**Risk:** CLI output is the primary user interface. Rendering bugs affect UX.

**Recommended Tests:**
```rust
// Test output capture for CLI renderer
#[test]
fn test_render_task_started() { }

#[test]
fn test_render_task_output_stdout() { }

#[test]
fn test_render_task_cache_hit() { }

#[test]
fn test_render_ci_context() { }

// JSON renderer serialization tests
#[test]
fn test_json_render_task_event() { }

#[test]
fn test_json_event_structure() { }
```

### 5. **Release Management E2E** (crates/release)

**Current State:** 40+ unit tests but no end-to-end tests.

**Risk:** Version bumping, changelog generation, and publishing involve file I/O coordination that unit tests don't cover.

**Recommended Tests:**
```rust
// crates/release/tests/integration_tests.rs
#[test]
fn test_version_bump_cargo_toml() { }

#[test]
fn test_version_bump_package_json() { }

#[test]
fn test_changelog_generation_from_commits() { }

#[test]
fn test_changeset_file_processing() { }

#[test]
fn test_monorepo_version_sync() { }
```

---

## Testing Infrastructure Improvements

### 1. Add Test Coverage Reporting

Add cargo-llvm-cov or grcov to CI:
```toml
# Cargo.toml dev-dependencies
[workspace.metadata.coverage]
exclude = ["crates/*/tests/*", "benches/*"]
```

### 2. Add Mocking Infrastructure for HTTP APIs

```toml
# Cargo.toml
[dev-dependencies]
wiremock = "0.6"  # For mocking GitHub/GitLab APIs
```

### 3. Add Integration Test Fixtures for CI

Create `crates/ci/tests/fixtures/` with:
- Sample project configurations
- Mock changed file lists
- Expected task execution orders

### 4. Property-Based Testing Expansion

Extend proptest usage from cuengine to:
- Task graph cycle detection
- Glob pattern matching
- Version parsing/serialization

---

## Specific Test Gaps by Risk

### High Risk (Should Fix First)
1. `dagger/src/lib.rs` - Container execution with no tests
2. `ci/src/affected.rs` - Task filtering logic with no tests
3. `ci/src/executor.rs` - CI orchestration with no tests
4. `github/src/ci.rs` - API interactions with minimal tests

### Medium Risk
1. `events/src/renderers/cli.rs` - User-facing output
2. `events/src/renderers/json.rs` - Machine-readable output
3. `release/` - Version management E2E
4. `ci/src/discovery.rs` - Project discovery

### Lower Risk (Nice to Have)
1. `ignore/src/lib.rs` - Simple file generation
2. `codeowners/` - CODEOWNERS generation
3. `gitlab/`, `bitbucket/` - Less commonly used providers

---

## Recommended Next Steps

1. **Immediate:** Add basic unit tests to `dagger/src/lib.rs`
2. **Week 1:** Add tests for `ci/affected.rs` with fixture data
3. **Week 2:** Add mocked integration tests for `github/src/ci.rs`
4. **Week 3:** Add CLI renderer tests with output capture
5. **Ongoing:** Set up coverage reporting in CI

---

## Test Commands Reference

```bash
# Run all tests
cuenv task test.unit

# Run specific crate tests
cuenv exec -- cargo test -p cuenv-ci
cuenv exec -- cargo test -p cuenv-dagger
cuenv exec -- cargo test -p cuenv-github

# Run with coverage (if configured)
cuenv exec -- cargo llvm-cov --workspace

# Run integration tests only
cuenv exec -- cargo test --test '*'
```
