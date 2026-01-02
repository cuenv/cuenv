# Test Coverage Review Checklist

This checklist tracks systematic review of each crate's test coverage.
Updated automatically by the Ralph Wiggum coverage script.

## Review Criteria

For each crate, verify:
1. **Existing tests are meaningful** - not just smoke tests
2. **Critical paths are covered** - error handling, edge cases
3. **Tests match the code's intent** - testing behavior, not implementation
4. **No missing test scenarios** - happy path, error path, boundary conditions

## Crate Review Status (sorted by coverage, lowest first)

| Crate | Coverage | Reviewed | Tests Added | Notes |
|-------|----------|----------|-------------|-------|
| dagger | 0.0% | [x] | [x] | Added 8 tests for constructor, accessors, and factory |
| aws | 11.8% | [x] | [x] | Added 15 tests for config, JSON extraction, error handling |
| 1password | 18.1% | [x] | [x] | Added 32 tests for config, WASM utils, host functions, resolver |
| vault | 25.8% | [x] | [x] | Added 12 tests for config, path handling, serialization |
| homebrew | 43.6% | [x] | [x] | Added 23 tests for formula generation, config, backend |
| gcp | 44.4% | [x] | [x] | Added 17 tests for config, resource name parsing, resolver |
| tools/oci | 52.8% | [x] | [x] | Added 34 tests for error, platform, cache modules |
| cuenv | 53.9% | [x] | [x] | Added 38 tests: coordinator protocol (16), discovery (6), CLI (16) |
| ignore | 54.3% | [x] | [x] | Added 18 tests for builder, validation, error types |
| secrets | 72.5% | [x] | [x] | Added 14 tests for SecretError, SecretSpec in lib.rs |
| editorconfig | 72.9% | [x] | [x] | Added 19 tests for optional builders, file ops, error handling |
| events | 74.0% | [x] | [x] | Added 54 tests: bus (18), event (36 for all event types) |
| ci | 74.2% | [x] | [x] | Added 26 tests: context (7), flake/error (10), provider/local (9) |
| github | 75.4% | [x] | [x] | Added 10 tests: config (Default, serde, merge, permissions) |
| release | 77.2% | [x] | [x] | Added 12 tests: backends (BackendContext, PublishResult builders) |
| cubes | 79.8% | [x] | [x] | Added 60 tests: lib.rs (5 error types), cube.rs (24 for FileMode, FormatConfig, accessors, CUE loading), formatter.rs (12 for all languages, edge cases), generator.rs (12 for scaffold/managed modes, check mode), config.rs (15 for biome/prettier/rustfmt configs) |
| core | 83.6% | [x] | [x] | Large crate, already well-tested. Added 12 tests in shell.rs (detect, serde, Default, case-insensitive parsing, env vars). Existing tests are comprehensive for Error types, paths, module, tasks, hooks. |
| workspaces | 84.3% | [x] | [x] | Already well-tested with 665+ lines of tests in types.rs, 400+ in error.rs. Coverage is comprehensive for Workspace, PackageManager, DependencySpec, LockfileEntry, all error variants, and serde roundtrips. |
| buildkite | 87.0% | [x] | [x] | Added 22 tests in schema.rs for Pipeline, CommandStep, BlockStep, WaitStep, GroupStep, AgentRules, DependsOn, RetryConfig serialization. Already had 17 tests in emitter.rs and provider.rs. |
| cuengine | 87.6% | [x] | [x] | Already well-tested with 55 tests. Added 14 tests in validation.rs (Limits, path validation, package validation), 5 tests in retry.rs (RetryConfig, with_retry success/failure), 6 tests in cache.rs (capacity error, is_empty, clear, key combinations). |
| codeowners | 90.4% | [x] | [N/A] | Already above 80% target, no changes needed |
| bitbucket | 94.5% | [x] | [N/A] | Already above 80% target, no changes needed |
| gitlab | 95.2% | [x] | [N/A] | Already above 80% target, no changes needed |

## Session Log

<!-- Claude will append notes here as it reviews each crate -->

### 2026-01-02: Coverage improvement session

**Starting coverage: 71.89%**
**Ending coverage: 73.05%**

#### Tests Added:

1. **ci/src/affected.rs** (0% -> high coverage)
   - Added 33 tests covering: `matches_any` (glob matching, prefix matching, edge cases), `matched_inputs_for_task`, `compute_affected_tasks` (direct match, transitive deps, external deps, pipeline ordering), `is_task_directly_affected`, `check_external_dependency` (caching, circular prevention)

2. **core/src/rules/discovery.rs** (0% -> moderate coverage)
   - Added 19 tests covering: `RulesDiscovery` construction, `discover` method (empty dirs, eval failures, clears previous results), `DiscoveredRules` struct, `RulesDiscoveryError` variants, `load_rules` function

3. **core/src/secrets/mod.rs** (8.64% -> high coverage)
   - Added 25 tests covering: `ExecResolver` (construction, serde, clone, eq), `Secret` construction (`new`, `onepassword`, `with_extra`), `provider` method, `to_spec` method, serde roundtrips (skip empty fields, extra fields)

4. **workspaces/src/resolver.rs** (0% -> 96.08%)
   - Added 20 tests covering: `detect_workspace_protocol` (JS workspace:*, version, Rust workspace), `resolve_external_deps` (filtering workspace members), `resolve_dependencies` (graph creation, edges, missing deps, multiple versions), `parse_js_deps` (workspace deps, regular deps, empty), `parse_rust_deps` (workspace deps, non-workspace deps)

5. **workspaces/src/materializer/cargo_deps.rs** (0% -> covered)
   - Added 5 tests covering: skip non-Cargo workspaces, create target dir symlinks, replace existing symlinks, replace existing directories, work with pre-existing workspace targets

6. **workspaces/src/materializer/node_modules.rs** (0% -> covered)
   - Added 15 tests covering: handle all JS package managers (npm, bun, pnpm, yarn classic/modern), skip Cargo projects, handle missing source node_modules, skip if target exists, detect_cache_dir for all package managers

7. **tools/github/src/lib.rs** (22.79% -> improved)
   - Added 28 tests covering: ToolProvider trait impl (name, description, default), expand_template (all OS/arch combos), tool_cache_dir, get_effective_token (priority, fallbacks), RateLimitInfo (default, format methods), build_api_error (rate limit, 403, 404, 401, 500), is_cached, Release/Asset deserialization

8. **ci/src/executor/mod.rs** (21.85% -> improved)
   - Added 19 tests covering: ExecutorError variants (display messages), PipelineResult fields, CIExecutor construction, has_custom_cache_backend, cache_backend_name, CIExecutorConfig builder, make_simple_ir/make_task helpers, extract_fingerprints, TaskOutput methods
