---
title: "ADR: Hermetic, Input-Addressed Task Execution with Persistent Cache"
description: "Hermetic task workspaces, content-addressed cache keys, and cache layout."
---

Status: Accepted
Date: 2025-10-07

## Context

Tasks must execute deterministically from a set of explicitly declared inputs and produce a set of declared outputs. To enable reproducibility and performance, we want a persistent, content-addressed cache that skips reruns when inputs and execution context are identical. Hermeticity here refers to a directory-only isolation model: the task runs in a clean working directory populated solely from declared inputs.

## Decisions

1. Inputs/Outputs syntax

- Accept files, directories, and glob patterns (e.g., `src/**/*.ts`) relative to the env.cue root.
- Globs are first-class for both inputs and outputs.

2. Hermetic execution

- Each task runs in a fresh working directory pre-populated only with its resolved inputs.
- Directory-only isolation; no network isolation.
- Symlinks are resolved to target content at population time. Hardlinks are used when possible, falling back to copies on cross-device or FS limitations.

3. Cache key

- SHA256 over a canonical JSON envelope containing:
  - Sorted list of `{path, content_hash}` for all resolved input files
  - `command`, `args`, and `shell` (if any)
  - Resolved environment variables for the task (after env policies)
  - `cuenv` version and platform triple
- Full hex digest used as the key.

4. Cache behavior and storage

- On key hit: skip execution entirely and log a cache-hit message. Outputs are not materialized by default.
- Cache is persisted under `~/.cuenv/cache/tasks/<key>/` containing:
  - `metadata.json` (canonical envelope and execution metadata)
  - `outputs/` tree with declared outputs only
  - `logs/` for stdout and stderr (captured when available)
  - `workspace.tar.zst` snapshot of the full hermetic workspace
- No GC; manual cleanup only.

5. Outputs and undeclared writes

- Only declared outputs are indexed and persisted to `outputs/`.
- Writes to undeclared paths are allowed but produce a WARN log and are not cached as outputs.
- No implicit coupling: a task must explicitly list the outputs of its dependencies as inputs.

6. CLI UX

- On hit: `Task <name> cache hit: <key>. Skipping execution.`
- On miss: `Task <name> executing hermetically… key <key>`
- Flags added (no behavior change by default):
  - `--materialize-outputs <dir>`: copy cached outputs into `dir` on cache hit
  - `--show-cache-path`: print the cache path `~/.cuenv/cache/tasks/<key>`

## Consequences

- Deterministic, reproducible execution based on explicit inputs.
- Large performance wins from skipping reruns on unchanged inputs.
- Clear separation between indexed outputs and incidental writes in the workspace.
- Simpler mental model for dependencies: no implicit output injection.

## Future Work

- Optional network sandboxing or policy controls.
- Cache GC policies and pruning tools.
- Partial materialization strategies and remote cache backends.
