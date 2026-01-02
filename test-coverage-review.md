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

| Crate        | Coverage | Reviewed | Tests Added | Notes                                                                                                                                                                                                                                                                 |
| ------------ | -------- | -------- | ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1password    | 45.76%   | [x]      | [x]         | Added 41 tests for config, WASM utils, host functions, resolver, OS/arch mapping                                                                                                                                                                                      |
| cuenv        | 55.60%   | [x]      | [x]         | Added 38 tests: coordinator protocol (16), discovery (6), CLI (16). Large crate requiring integration testing.                                                                                                                                                        |
| aws          | 62.28%   | [x]      | [x]         | Added 32 tests for config, JSON extraction, error handling, resolver, batch operations                                                                                                                                                                                |
| dagger       | 65.29%   | [x]      | [x]         | Added 20 tests for constructor, accessors, factory, cache sharing, path handling                                                                                                                                                                                       |
| tools/oci    | 71.39%   | [x]      | [x]         | Added 34 tests for error, platform, cache modules                                                                                                                                                                                                                     |
| vault        | 72.30%   | [x]      | [x]         | Added 26 tests for config, path handling, serialization, mount options, resolver                                                                                                                                                                                       |
| ignore       | 73.52%   | [x]      | [x]         | Added 18 tests for builder, validation, error types                                                                                                                                                                                                                   |
| github       | 76.43%   | [x]      | [x]         | Added 10 tests: config (Default, serde, merge, permissions)                                                                                                                                                                                                           |
| gcp          | 77.09%   | [x]      | [x]         | Added 17 tests for config, resource name parsing, resolver                                                                                                                                                                                                            |
| release      | 79.65%   | [x]      | [x]         | Added 12 tests: backends (BackendContext, PublishResult builders)                                                                                                                                                                                                     |
| ci           | 82.48%   | [x]      | [x]         | Added 26 tests: context (7), flake/error (10), provider/local (9)                                                                                                                                                                                                     |
| homebrew     | 83.80%   | [x]      | [x]         | Added 23 tests for formula generation, config, backend                                                                                                                                                                                                                |
| events       | 84.15%   | [x]      | [x]         | Added 54 tests: bus (18), event (36 for all event types)                                                                                                                                                                                                              |
| secrets      | 84.23%   | [x]      | [x]         | Added 14 tests for SecretError, SecretSpec in lib.rs                                                                                                                                                                                                                  |
| core         | 85.36%   | [x]      | [x]         | Large crate, already well-tested. Added 12 tests in shell.rs (detect, serde, Default, case-insensitive parsing, env vars). Existing tests are comprehensive for Error types, paths, module, tasks, hooks.                                                             |
| workspaces   | 89.24%   | [x]      | [x]         | Already well-tested with 665+ lines of tests in types.rs, 400+ in error.rs. Coverage is comprehensive for Workspace, PackageManager, DependencySpec, LockfileEntry, all error variants, and serde roundtrips.                                                         |
| codeowners   | 90.37%   | [x]      | [N/A]       | Already above 80% target, no changes needed                                                                                                                                                                                                                           |
| buildkite    | 90.60%   | [x]      | [x]         | Added 22 tests in schema.rs for Pipeline, CommandStep, BlockStep, WaitStep, GroupStep, AgentRules, DependsOn, RetryConfig serialization. Already had 17 tests in emitter.rs and provider.rs.                                                                          |
| cuengine     | 91.17%   | [x]      | [x]         | Already well-tested with 55 tests. Added 14 tests in validation.rs (Limits, path validation, package validation), 5 tests in retry.rs (RetryConfig, with_retry success/failure), 6 tests in cache.rs (capacity error, is_empty, clear, key combinations).             |
| bitbucket    | 94.49%   | [x]      | [N/A]       | Already above 80% target, no changes needed                                                                                                                                                                                                                           |
| gitlab       | 95.16%   | [x]      | [N/A]       | Already above 80% target, no changes needed                                                                                                                                                                                                                           |
| editorconfig | 98.19%   | [x]      | [x]         | Added 19 tests for optional builders, file ops, error handling                                                                                                                                                                                                        |
| cubes        | 98.88%   | [x]      | [x]         | Added 60 tests: lib.rs (5 error types), cube.rs (24 for FileMode, FormatConfig, accessors, CUE loading), formatter.rs (12 for all languages, edge cases), generator.rs (12 for scaffold/managed modes, check mode), config.rs (15 for biome/prettier/rustfmt configs) |

## Session Log

<!-- Claude will append notes here as it reviews each crate -->

### 2026-01-02: Coverage improvement session (third pass)

**Starting coverage: 74.83%**
**Ending coverage: 75.06%** (+0.23%)

#### Tests Added (this session):

1. **1password/src/secrets/core.rs** (42.22% -> 45.76%)
   - Added 9 tests: OS mapping (linux, windows), arch mapping (arm, riscv), host_functions creation, shared_core mutex, get_or_init without WASM

2. **aws/src/secrets.rs** (42.62% -> 62.28%)
   - Added 17 tests: config equality/deserialization/roundtrip, extract_json_key edge cases (empty string, special chars, unicode, numeric, float), resolver without credentials, provider_name, supports_native_batch, debug output, resolve_batch empty, http_credentials_available logic

3. **dagger/src/lib.rs** (47.50% -> 65.29%)
   - Added 12 tests: container_cache is shared, project_root paths, default_image variants, factory extracts image, cache multiple containers, backend_options no image, cloned cache, empty image, backend_config type field, backend_options both fields

4. **vault/src/secrets.rs** (53.85% -> 72.30%)
   - Added 14 tests: config different mounts, roundtrip, full_path special chars/empty/unicode, deserialization errors, resolver without credentials, provider_name, debug output, http_credentials_available, path with slashes, empty key, mount partial match

---

### 2026-01-02: Coverage improvement session (continued)

**Starting coverage: 73.03%**
**Ending coverage: 74.33%** (+1.3%)

#### Tests Added (earlier session):

9. **secrets/src/resolved.rs** (19% -> improved)
   - Added 17 tests covering: `ResolvedSecrets::new()`, `is_empty()`, `get()`, `fingerprint_matches()` with current/previous salt, no salt configured, missing secrets, `compute_fingerprints_for_validation()` with both/only-current/only-previous/no salts, clone, debug

10. **events/src/renderers/json.rs** (0% -> 100%)
    - Added 8 tests covering: `JsonRenderer::new()`, `pretty()`, `default()`, debug, `render_to_string()` compact and pretty output, JSON validity verification

11. **ci/src/report/json.rs** (0% -> 100%)
    - Added 9 tests covering: `write_report()` creates valid JSON, pretty-prints, includes context and tasks, failed/cached/skipped task states, nested directories, invalid path error, multiple tasks

12. **ci/src/diff.rs** (55% -> ~90%)
    - Added 22 tests covering: all `DiffError` variants, task added/removed detection, `CacheInvalidated` change type, summary counts, `format_diff()` with SHAs/secrets/added-removed, `compare_runs()` success and error, `load_report()` invalid JSON, `find_first_report()` success/no-json/not-exists, `compare_by_sha()`, `DigestDiff` serialization, `ChangeType` serialization, `DiffSummary` default, no cache keys, short SHAs

---

### 2026-01-02: Coverage improvement session (earlier)

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
   - Added 20 tests covering: `detect_workspace_protocol` (JS workspace:\*, version, Rust workspace), `resolve_external_deps` (filtering workspace members), `resolve_dependencies` (graph creation, edges, missing deps, multiple versions), `parse_js_deps` (workspace deps, regular deps, empty), `parse_rust_deps` (workspace deps, non-workspace deps)

5. **workspaces/src/materializer/cargo_deps.rs** (0% -> covered)
   - Added 5 tests covering: skip non-Cargo workspaces, create target dir symlinks, replace existing symlinks, replace existing directories, work with pre-existing workspace targets

6. **workspaces/src/materializer/node_modules.rs** (0% -> covered)
   - Added 15 tests covering: handle all JS package managers (npm, bun, pnpm, yarn classic/modern), skip Cargo projects, handle missing source node_modules, skip if target exists, detect_cache_dir for all package managers

7. **tools/github/src/lib.rs** (22.79% -> improved)
   - Added 28 tests covering: ToolProvider trait impl (name, description, default), expand_template (all OS/arch combos), tool_cache_dir, get_effective_token (priority, fallbacks), RateLimitInfo (default, format methods), build_api_error (rate limit, 403, 404, 401, 500), is_cached, Release/Asset deserialization

8. **ci/src/executor/mod.rs** (21.85% -> improved)
   - Added 19 tests covering: ExecutorError variants (display messages), PipelineResult fields, CIExecutor construction, has_custom_cache_backend, cache_backend_name, CIExecutorConfig builder, make_simple_ir/make_task helpers, extract_fingerprints, TaskOutput methods

### 2026-01-02: Coverage improvement session (fourth pass)

**Starting coverage: 74.53%**
**Ending coverage: 75.64%** (+1.11%)

#### Tests Added (this session):

1. **release/src/orchestrator.rs** (19.85% -> improved)
   - Added 23 tests: ReleasePhase enum, OrchestratorConfig builder methods, ReleaseReport lifecycle and backend results, ReleaseOrchestrator artifact loading and dry-run modes

2. **tools/oci/src/registry.rs** (45.87% -> improved)
   - Added 14 tests: parse_reference with various formats and registries, compute_file_digest with various content sizes, OciClient authentication and ResolvedImage

3. **secrets/src/batch.rs** (60.50% -> improved)
   - Added 18 tests: BatchConfig construction and cloning, BatchResolver with multiple resolvers, resolve_batch with salt requirements and cache keys

4. **release/src/manifest.rs** (65.87% -> improved)
   - Added 16 tests: workspace version reading and error handling, package dependency tracking and glob patterns, workspace dependency version updates

5. **core/src/base/discovery.rs** (43.42% -> improved)
   - Added 17 tests: derive_synthetic_name with various path scenarios, BaseDiscovery construction and with_eval_fn builder, discovery with single/nested/multiple env.cue files, skipping failed loads, respecting .gitignore, DiscoveredBase struct, DiscoveryError variants

6. **core/src/cache/tasks.rs** (67.98% -> improved)
   - Added 19 tests: OutputIndexEntry and TaskResultMeta serialization, CacheEntry and TaskLatestIndex types, CacheKeyEnvelope with optional fields, key_to_path, lookup, record_latest, lookup_latest, get_project_cache_keys

7. **cuenv/src/provider.rs** (53.23% -> improved)
   - Added 16 tests: Provider trait methods (as_any, as_any_mut, downcast), SyncMode enum (debug, clone, equality), SyncOptions struct, SyncResult methods

8. **ci/src/discovery.rs** (60.99% -> improved)
   - Added 7 tests: DiscoveredCIProject clone/debug, find_cue_module_root edge cases

---
