---
title: Schema status
description: Current implementation status for cuenv CUE schema definitions
---

The CUE schema is the first source of truth for cuenv features. The implementation status lives in [`docs/design/specs/schema-coverage-matrix.md`](../../../../../design/specs/schema-coverage-matrix.md), which has one row for every exported `#Definition` in `schema/**/*.cue`.

Agents and contributors must use the matrix before generating examples or answering feature-support questions. A schema definition can be real without being fully implemented.

## Status meanings

- `implemented`: safe to recommend with current docs and examples.
- `partial`: usable only with the documented limitations.
- `schema-only`: do not recommend as working behavior.
- `legacy`: avoid in new examples unless explaining compatibility.
- `internal`: implementation helper, not usually a user-facing feature.
- `docs-misleading`: current docs overclaim support and must be corrected.
- `needs-decision`: intended user surface is not yet clear.

## High-risk current gaps

- Secrets: `#ExecSecret` and feature-gated `#OnePasswordRef` are implemented; `#AwsSecret`, `#GcpSecret`, and `#VaultSecret` are schema-only until runtime resolvers are registered.
- Runtime and tools: Nix, devenv, GitHub, Nix, Rustup, and URL tool sources are implemented; container, OCI runtime, and OCI tool source support are not complete.
- Images: `#ContainerImage` is schema-visible, but `cuenv build` currently validates/lists rather than building images.
- Services: `cuenv up`, `ps`, and basic logs exist, but `down`, `restart`, and `logs --follow` are partial.
- Tasks: groups, sequences, params, output refs, and caching are real; `timeout`, `retry`, `continueOnError`, and group `maxConcurrency` need explicit limitation notes.
- CI and release: GitHub CI sync is the strongest path; GitLab export/sync and config-driven release backends are not complete.

Run `cuenv task ci.schema-docs-check` after changing schema, docs, prompts, examples, skills, or CLI surfaces.

