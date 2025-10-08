# Cross-project task dependencies (monorepo, hermetic, cached)

Summary
- Enable tasks to select and consume outputs from tasks in other projects (subdirectories) of the same Git monorepo.
- Integrates with hermetic execution and input-based caching: only explicitly mapped files/dirs become inputs to the dependent task’s cache key.
- Enforces monorepo-only scope with strict path safety and clear error messages.

Why
- Unblocks multi-project pipelines where one project builds shared artifacts (docs, assets, binaries) and another consumes them deterministically, without bleeding environments or non-hermetic state.

Scope & UX
- Local filesystem only within same Git root; no Git URLs in v1.
- Reference syntax: project path + task, e.g. "../projB:build" or absolute-from-repo-root "/projB:build".
- Auto-run policy (default): if external task cache is missing, the external task runs in its project hermetically and populates cache.
- Explicit mapping: dependent task must map selected outputs to unique destinations in its hermetic workspace; collisions fail.
- External tasks use their own environment; no injection from dependent.

Schema updates
- tasks.cue: added `externalInputs?: [...#ExternalInput]` with:
  - `#ExternalInput { project: string, task: string, map: [...#Mapping] }`
  - `#Mapping { from: string, to: string }`
- Documented rules: `from` must be declared output; directories map recursively; destinations must be unique; monorepo-only path safety.
- Regenerated JSON schema from Rust types.

Core changes (cuenv-core)
- Added `Mapping` and `ExternalInput` types; added `Task.external_inputs` (serde rename `externalInputs`).
- `ExecutorConfig` now supports optional `working_dir` to set process cwd for hermetic runs.
- Boxed `TaskDefinition::Single(Box<Task>)` to satisfy clippy (large enum variant).

CLI flow (cuenv-cli)
- For Single tasks without hermetic features (no `externalInputs`, no `inputs`, no `outputs`), behavior is unchanged; dependency graph execution remains the same.
- For tasks using hermetic features:
  1) Discover Git root by walking up to `.git`; fail if not found.
  2) Resolve external project path (absolute from root or relative to env.cue), canonicalize, and enforce repo-root containment; fail if outside.
  3) Evaluate external project (auto-detect package by scanning `.cue` files) and locate the external task.
  4) Validate mappings: `from` must be a declared output of the external task; detect destination collisions.
  5) Compute external task cache key from command + args + environment for that task + hashes of its declared `inputs`; check `~/.cuenv/cache/tasks/<key>/outputs/`.
  6) Auto-run external task on cache miss inside a hermetic workspace (its own env), then store declared outputs under the cache/outputs directory.
  7) Materialize only selected mapped outputs into the dependent task’s hermetic workspace (hardlink fallback copy).
  8) Materialize dependent local `inputs` into the hermetic workspace.
  9) Compute dependent cache key by hashing materialized external files + local inputs + command/args/env; if cache hit, skip execution; else run hermetically and store declared dependent `outputs` into cache.
- Logging: resolving external task, cache hit/miss, auto-run, and mapping per file/dir.

Safety & failures
- Outside Git root → hard failure with actionable message.
- External task not found / not single → failure.
- Mapping references non-declared output → failure.
- Destination collisions → failure.
- External task failed → failure.

Caching rules
- Dependent cache key includes only content hashes of materialized external files (post-selection) + local `inputs` + command/args/env.
- External task cache key is independent; we do NOT include external metadata/keys into the dependent’s key.

Tests (integration)
- New suite `crates/cuenv-cli/tests/cross_project_deps.rs`:
  - First run: auto-runs external, then dependent succeeds (materialized vendor/app.txt verified by usage).
  - Second run: cache hit for both; no re-execution.
  - Invalidation: change external input → dependent cache invalidated by file hash change.
  - Mapping error: non-declared output → clear failure.
  - Path safety: external project outside Git root → hard failure.
  - Collision: two mappings target same destination → failure.
- All existing CLI/core tests continue to pass.

Docs
- ADR: `docs/adrs/adr-cross-project-deps.md` documents monorepo-only scope, syntax, mapping semantics, auto-run, error cases, and cache-key rules.

Backward compatibility
- If a task doesn’t use `externalInputs`, `inputs`, or `outputs`, the original executor and graph behavior is preserved.
- This keeps current users unaffected while enabling new hermetic flows.

Known gaps / future work
- JSON schema naming vs serde renames: schemars still exports some field names in general-purpose form.
- Potential future: glob patterns for inputs/outputs with deterministic expansion; parallel hermetic runs with strong isolation per node; better debug output for cache hashing.

How to review
- Start with ADR for context.
- Review schema, types, and CLI flow.
- Exercise the integration tests locally; also try a small two-project monorepo scenario.

Acceptance criteria mapping
- External references by project path + task: implemented.
- Auto-run external on cache miss: implemented with hermetic workspace, own env.
- Explicit mapping of declared outputs only, unique destinations: enforced.
- Monorepo-only path safety: enforced with Git root discovery + canonicalization.
- Dependent cache key includes only materialized external files (+ local inputs): implemented.
- Clear failures for outside root, task not found, non-declared output, collisions, external failure: implemented.
