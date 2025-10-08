# ADR: Cross-Project Task Dependencies (Monorepo-only)

Status: Accepted
Date: 2025-10-07

## Context

We want to allow tasks in one project of a Git monorepo to depend on and consume outputs from tasks defined in other projects (subdirectories) of the same repository. This must integrate with the hermetic execution model and input-based caching so that only explicitly materialized files/dirs from external tasks are included as inputs to the dependent task. Security and safety constraints require we never traverse outside the Git root and that external tasks execute with their own environment, not that of the dependent.

## Decisions

- Scope: Local filesystem only, monorepo-only. All external project paths must resolve within the Git root. No Git URLs in v1.
- Reference syntax: External references specify a project path and task name, with project paths either absolute-from-git-root (prefix "/") or relative to the env.cue which declares the dependency.
- Auto-run: If the external task’s cached outputs are missing, it is automatically run (with hermetic execution) in its own project to populate cache.
- Selection and mapping: The dependent task must explicitly map a subset of the external task’s declared outputs to unique destinations in its hermetic workspace. No implicit inclusion.
- Environment: External tasks run with their own project’s environment; no injection or overrides from the dependent task.
- Failure behavior: Hard fail with actionable errors when outside repo root, unknown tasks, undeclared outputs referenced, destination collisions, or external execution fails.
- Cache keying: The dependent task’s cache key includes only the content hashes of the explicitly materialized files/dirs from external tasks, the local inputs, the command/args, and the effective environment for that task. External task metadata or keys are not included.

## Schema

Added to schema/tasks.cue:

- `externalInputs?: [...#ExternalInput]`
- `#ExternalInput { project: string, task: string, map: [...#Mapping] }`
- `#Mapping { from: string, to: string }`

Rules:
- `from` values must be among the external task’s declared outputs. Directories map recursively.
- Each `to` destination must be unique; collisions are disallowed.

Generated JSON schema has been regenerated from Rust types; future iteration will align serde renames.

## Implementation

- Types: Added `Mapping` and `ExternalInput` types in `cuenv-core::tasks`, and `externalInputs` field on `Task`.
- Executor support: `ExecutorConfig` now supports an optional `working_dir` to allow hermetic execution in a workspace directory.
- CLI integration: Implemented hermetic resolution and materialization in `cuenv-cli` task flow:
  - Git root discovery by walking up to `.git` from the project dir.
  - Safe path resolution to ensure external project paths remain within the repo root.
  - External manifest evaluation using cuengine with package auto-detection from `.cue` files.
  - External cache key computed from external task command/args/env and declared `inputs` content.
  - Auto-run external task on cache miss in a hermetic workspace; cache declared outputs under `~/.cuenv/cache/tasks/<key>/outputs/`.
  - Validate mappings: `from` must be a declared output; destinations must be unique.
  - Materialize only selected external outputs into the dependent task’s hermetic workspace (hardlink, fallback to copy).
  - Include materialized external files and local `inputs` in the dependent task’s cache key; on cache hit, skip execution.
  - Store dependent declared outputs in its own cache directory on success.
  - Logging shows external resolution, cache hit/miss, auto-run, and mapping actions.

## Errors

Clear actionable errors are returned for:
- Git root not found or path resolves outside repo root
- External task not found or not a single task
- Mapping refers to non-declared output
- Destination collisions in mappings
- External task failed execution
- Cache entry missing after run (unexpected)

## Tests

Integration tests (`crates/cuenv-cli/tests/cross_project_deps.rs`) cover:
- First run: external task auto-runs; dependent materializes vendor/app.txt; task succeeds
- Second run: cache hits for both external and dependent; no re-execution
- Invalidation: changing external input file content invalidates dependent via file-hash changes
- Mapping error: referencing non-declared output fails
- Path safety: resolving external project outside git root fails
- Collision: two mappings target the same destination fails

## Future Work

- Promote hermetic/caching logic into shared core once the engine boundaries are finalized (avoid cycles with cuengine)
- Improve JSON schema generation to reflect serde `rename` fields exactly
- Add glob support for `inputs` and `outputs` with deterministic expansion and hashing
- Parallelize hermetic group execution with proper workspace isolation per task
