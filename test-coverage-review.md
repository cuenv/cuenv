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

### 2026-01-02: Coverage improvement session (fifth pass)

**Starting coverage: 74.94%**
**Ending coverage: 75.80%** (+0.86%)

#### Tests Added (this session):

1. **ci/src/executor/cache.rs** (63.79% -> improved)
   - Added 30+ tests for LocalCacheBackend:
   - CacheError variants and io_with_context helper
   - CacheResult and CacheMetadata serialization
   - TaskLogs default and content handling
   - cache_path_for_digest with short digests
   - load_metadata and load_logs error paths
   - LocalCacheBackend async operations (check, store, get_logs, restore_outputs)
   - walkdir and is_file_executable utilities
   - store_result with various log configurations

2. **ci/src/executor/remote.rs** (15.86% -> improved)
   - Added 10+ tests for RemoteCacheConfig:
   - from_env with URL, instance, TLS settings
   - TLS parsing with "true", "1", "false"
   - Empty URL handling
   - Config default values
   - compute_digest determinism
   - to_bazel_digest prefix handling
   - is_retryable for all gRPC status codes
   - RemoteCacheBackend construction and debug
   - create_backoff configuration

3. **core/src/tasks/executor.rs** (57.71% -> improved)
   - Added 30+ tests for helper functions and types:
   - summarize_task_failure with exit codes, no exit code, no output, truncation
   - summarize_stream for empty, whitespace, and short content
   - format_failure_streams with both stdout and stderr
   - find_workspace_root for npm, pnpm, cargo, deno
   - package_json_has_workspaces (array, object, empty, missing)
   - cargo_toml_has_workspace detection
   - deno_json_has_workspace (array, object styles)
   - ExecutorConfig field coverage
   - TaskResult clone and constant checks

4. **core/src/tasks/io.rs** (coverage improved)
   - Added 25+ tests for IO utilities:
   - ResolvedInputFile and ResolvedInputs fields and cloning
   - to_summary_map output
   - normalize_rel_path with ./ and ../
   - sha256_file including empty files and not found
   - extract_glob_base with various patterns
   - InputResolver edge cases (empty, whitespace, missing, deduplication)
   - collect_outputs (empty, files, directory patterns, sorting)
   - snapshot_workspace_tar_zst
   - populate_hermetic_dir nested directories
   - Absolutize trait for relative and absolute paths

5. **ci/src/executor/backend.rs** (coverage improved)
   - Added 10+ tests for backend types:
   - CacheLookupResult clone
   - CacheOutput fields and executability
   - CacheEntry fields and with outputs
   - BackendError display messages for all variants
   - BackendError from_io conversion
   - Clone implementations for data types

---

### 2026-01-02: Coverage improvement session (seventh pass)

**Starting coverage: 76.35%**
**Ending coverage: 76.92%** (+0.57%)

#### Tests Added (this session):

1. **events/src/metadata.rs** (improved coverage)
   - Added 6 tests: Default trait, with_correlation_id, debug format, clone, with string target, set_correlation_id after init

2. **release/src/error.rs** (79.63% -> improved)
   - Added 11 tests: changeset_io_with_source, all error constructor variants, From<io::Error>, error debug

3. **cuenv/src/builder.rs** (76.47% -> improved)
   - Added 4 tests: with_sync_provider, multiple providers, chaining, defaults then more

4. **cuenv/src/lib.rs** (improved coverage)
   - Added 6 tests: run with empty/default registry, EXIT_SIGINT constant, LLMS_CONTENT, sync command args, run_cli_with_registry

5. **cuenv/src/providers/ci.rs** (72.73% -> improved)
   - Added 5 tests: as_any_mut, command has args, Default trait, has_config

6. **cuenv/src/providers/cubes.rs** (67.69% -> improved)
   - Added 5 tests: as_any_mut, command has args, Default trait, has_config

7. **cuenv/src/tracing.rs** (improved coverage)
   - Added 12 tests: format parsing (all variants, case-insensitive), LogLevel conversion, TracingConfig (default, clone, debug), format/level debug and clone, subscribe_global_events

8. **cuenv/src/commands/task/arguments.rs** (60.66% -> improved)
   - Added 11 tests: boolean flag at end, short flag with value, negative number as positional, multi-char short as positional, resolve_task_args for all error paths, default values, optional without default, apply_args_to_task interpolation

9. **cuenv/src/commands/task/types.rs** (70.10% -> improved)
   - Added 18 tests: labels/interactive/all selection, with_backend, with_help, with_materialize_outputs, with_show_cache_path, OutputConfig/ExecutionMode/TaskSelection default and clone, request debug and clone, with_args on non-Named selection

10. **cuenv/src/providers/rules.rs** (improved coverage)
    - Added 7 tests: as_any_mut, command has args, Default, has_config, sync_codeowners empty/with rules, CUENV_HEADER constant

---

### 2026-01-02: Coverage improvement session (eighth pass)

**Starting coverage: 76.92%**
**Ending coverage: 77.51%** (+0.59%)

#### Tests Added (this session):

1. **cuenv/src/coordinator/client.rs** (coverage improved)
   - Added 7 tests: CoordinatorHandle::existing, ::new, clone, debug, socket with spaces, relative socket, existing vs new difference

2. **cuenv/src/completions.rs** (33.33% -> improved)
   - Added 11 tests: find_cue_module_root edge cases (nonexistent, no cue.mod, with cue.mod, subdirectory), get_available_tasks (empty path, invalid package), complete_task_params, get_task_params, task_completer

3. **events/src/renderers/cli.rs** (86.03% -> 96.13%)
   - Added 37 tests: CliRendererConfig (default, debug, clone), CliRenderer (new, default, with_config, debug), render methods for all event types (TaskEvent variants, CiEvent variants, CommandEvent, InteractiveEvent, SystemEvent, OutputEvent), verbose vs non-verbose modes

4. **cuenv/src/commands/info.rs** (20.51% -> improved)
   - Added 7 tests: ProjectInfo/InfoOutput/MetaOutput debug and serialization, multiple projects, execute_info error paths (invalid path, no CUE module)

5. **cuenv/src/commands/fmt.rs** (22.75% -> improved)
   - Added 7 tests: DiscoveredFiles (all types not empty, total count), should_include (empty filter, single formatter), execute_fmt/load_base_config error paths

---

### 2026-01-03: Coverage improvement session (ninth pass)

**Starting coverage: 76.70%**
**Ending coverage: 77.07%** (+0.37%)

#### Tests Added (this session):

1. **1password/src/secrets/wasm.rs** (coverage improved)
   - Added 7 tests: filename verification, path structure, error messages

2. **1password/src/secrets/resolver.rs** (coverage improved)
   - Added 18 tests: config edge cases, serialization, equality

3. **tools/github/src/lib.rs** (coverage improved)
   - Added 17 tests: format_reset_duration, extract_binary, find_main_binary, compute_file_sha256

4. **aws/src/secrets.rs** (coverage improved)
   - Added 16 tests: config serialization, JSON extraction edge cases

5. **dagger/src/lib.rs** (coverage improved)
   - Added 12 tests: cache, paths, threading, config edge cases

6. **vault/src/secrets.rs** (coverage improved)
   - Added 14 tests: path handling, mount names, config edge cases

7. **github/src/ci.rs** (coverage improved)
   - Added 13 tests: repo parsing, PR refs, path handling
   - Removed 5 flaky env var tests that caused race conditions

8. **ignore/src/lib.rs** (coverage improved)
   - Added 19 tests: pattern generation, validation edge cases

9. **gcp/src/secrets.rs** (coverage improved)
   - Added 18 tests: resource name parsing, config edge cases

---

### 2026-01-03: Coverage improvement session (tenth pass)

**Starting coverage: 77.59%**
**Ending coverage: 78.06%** (+0.47%)

#### Tests Added (this session):

1. **ci/src/report/mod.rs** (coverage improved)
   - Added 20 tests: TaskStatus serde, PipelineStatus serde, TaskReport roundtrip, ContextReport handling, PipelineReport cache_hits(), CheckHandle clone/debug

2. **core/src/config/mod.rs** (coverage improved)
   - Added 30+ tests: Config default/serde/clone, OutputFormat variants, TaskListFormat as_str, CacheMode serde, CuenvSource as_str/default, BackendConfig/BackendOptions, CommandsConfig hierarchy, full config roundtrip

3. **release/src/conventional.rs** (coverage improved)
   - Added 15 tests: levenshtein_distance, parse_calver, extract_version, ComparableVersion ordering, ConventionalCommit clone/debug, bump_type for perf, aggregate_bump with breaking, summarize edge cases

4. **buildkite/src/provider.rs** (coverage improved)
   - Added 15 tests: detect edge cases (false value, missing commit/source), get_base_ref with PR/default branch, context accessor, create/update/complete check lifecycle for all status types

5. **ci/src/executor/engine.rs** (coverage improved)
   - Added 12 tests: EngineConfig with_cache_policy, clone, debug, from CIExecutorConfig, EngineResult debug, extract_fingerprints with secrets

6. **events/src/layer.rs** (coverage improved)
   - Added 13 tests: task cache hit/miss/output/completed events, stderr output, CI context/changed_files events, system shutdown, unknown event type, missing required fields, visitor initial state

7. **Fixed clippy warnings** in test code:
   - Single-char patterns (ignore/lib.rs, aws/secrets.rs)
   - Unreadable literals (ci/report/mod.rs)
   - Format string inlining (release/conventional.rs)

---

### 2026-01-03: Coverage improvement session (eleventh pass)

**Starting coverage: 78.06%**
**Ending coverage: 79.19%** (+1.13%)

#### Tests Added (this session):

1. **cuenv/src/commands/task_list.rs** (coverage improved)
   - Added 50+ tests for formatters:
   - TextFormatter, RichFormatter, TablesFormatter, DashboardFormatter, EmojiFormatter output tests
   - Color method tests (cyan, dim, bold, green, yellow)
   - Empty data handling for all formatters
   - source_proximity edge cases (exact match, parent dir, unrelated)
   - format_category_name for all categories
   - infer_category_from_name with description patterns
   - get_category_emoji for all categories
   - Clone and Debug implementations
   - calculate_max_width with nested nodes

2. **cuenv/src/commands/task_picker.rs** (33.82% -> improved)
   - Added 15 tests for TaskPicker:
   - SelectableTask clone/debug
   - PickerResult debug
   - Empty tasks handling
   - Filter by name and description (case-insensitive)
   - Filter no match behavior
   - selected_task with empty/filtered results
   - select_previous/next on empty lists
   - Wrap-around navigation
   - run_picker empty tasks

3. **cuenv/src/tui/widgets/task_tree.rs** (31.17% -> improved)
   - Added 12 tests for TaskTreeWidget:
   - get_group_status empty/with tasks/all completed/with failure
   - TreeViewItem clone
   - TreeNodeType debug
   - render_tree_prefix deep nesting
   - parse_task_path with many dots
   - widget_render_empty_state
   - render_tree_item_all_type

4. **cuenv/src/tui/widgets/output_panel.rs** (22.89% -> improved)
   - Added 16 tests for OutputPanelWidget:
   - extract_display_name (full format, two parts, simple)
   - parse_task_path (all formats)
   - task_matches_filter (exact, group, empty group)
   - get_visible_tasks (all, filtered, specific task)
   - widget_render_empty
   - render_task_header_format

---

### 2026-01-02: Coverage improvement session (sixth pass)

**Starting coverage: 75.81%**
**Ending coverage: 76.12%** (+0.31%)

#### Tests Added (this session):

1. **tools/rustup/src/lib.rs** (coverage improved)
   - Added 20+ tests for RustupToolProvider:
   - Provider description and default behavior
   - Host triple mapping for all OS/arch combinations
   - ToolSource handling for GitHub, Nix, Oci sources
   - Digest computation with components, targets, profiles
   - Digest determinism and order sensitivity
   - Toolchain path generation (stable, nightly, versioned)
   - is_toolchain_installed for nonexistent toolchains
   - Resolve method with minimal config, toolchain, profile, components, targets

2. **tools/nix/src/lib.rs** (coverage improved)
   - Added 15+ tests for NixToolProvider:
   - Provider description and default behavior
   - Flake resolution with custom flakes
   - with_flakes edge cases (empty, single, overwrite)
   - ToolSource handling for Nix, GitHub, Rustup, Oci sources
   - is_cached behavior for Nix and non-Nix sources

3. **github/src/release.rs** (30.41% -> improved)
   - Added 15+ tests for GitHubReleaseBackend:
   - parse_github_remote SSH format without .git suffix
   - Non-GitHub remotes (Bitbucket)
   - Empty and partial URLs
   - Nested paths in URLs
   - Config defaults and builder methods
   - Config clone and debug
   - Backend creation and name

4. **github/src/ci.rs** (23.93% -> improved)
   - Added 25+ tests for GitHubCIProvider:
   - parse_repo edge cases (different names, empty, too many parts)
   - parse_pr_number for large numbers, zero, empty, refs_pull_only, invalid
   - Branch ref handling (feature branches, develop, tags)
   - get_before_sha filtering (null SHA, empty, valid)
   - CI provider detection (not GitHub Actions, false value)
   - Git diff output parsing logic

---
