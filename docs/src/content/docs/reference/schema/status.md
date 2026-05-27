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

- Secrets: `#ExecSecret`, feature-gated `#OnePasswordRef`, feature-gated `#InfisicalSecret`, and feature-gated `#AwsSecret` are implemented; `#GcpSecret` and `#VaultSecret` are schema-only until runtime resolvers are registered.
- Runtime and tools: Nix, devenv, GitHub, Nix, Rustup, URL, and OCI tool sources are implemented; container runtime is not complete. OCI runtime (`#OCIRuntime`) is partial: `cuenv sync lock` resolves digests and per-image `extract` paths into `cuenv.lock`, and the `#OCIActivate` hook (`cuenv runtime oci activate`) extracts those binaries and prepends them to `PATH`.
- Images: `#ContainerImage` is schema-visible, but `cuenv build` currently lists image definitions and rejects selected build requests until an execution backend exists.
- Services: `cuenv up`, `ps`, `down`, `restart`, and `logs --follow` use persisted service session state; service-to-service dependencies are honored and selected service starts include their service deps, but task/image dependencies in `services.*.dependsOn` are rejected until executor integration exists; service docs and runnable fixtures are still partial.
- Tasks: groups, sequences, params, output refs, and caching are real; `timeout`, `retry`, `continueOnError`, and group `maxConcurrency` need explicit limitation notes.
- CI and release: GitHub CI sync is the strongest path; Buildkite sync/export is partial; GitLab export/sync is schema-only and sync rejects it with a configuration error until an emitter exists; config-driven release backends are not complete.

Run `cuenv task ci.schema-docs-check` after changing schema, docs, prompts, examples, skills, or CLI surfaces.
