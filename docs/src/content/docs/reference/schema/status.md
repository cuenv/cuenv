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

- Secrets: `#ExecSecret`, feature-gated `#OnePasswordRef`, feature-gated `#InfisicalSecret`, feature-gated `#AwsSecret`, and feature-gated `#GcpSecret` are implemented; `#VaultSecret` is schema-only until a runtime resolver is registered. Project environment policies also gate `cuenv infrastructure` actions through `allowInfrastructure`, before secret resolution, and resolved secret parts are registered for redaction.
- Runtime and tools: Nix, devenv, GitHub, Nix, Rustup, URL, and OCI tool sources are implemented; container runtime is not complete. OCI runtime (`#OCIRuntime`) is partial: `cuenv sync lock` resolves digests and per-image `extract` paths into `cuenv.lock`, and the `#OCIActivate` hook (`cuenv runtime oci activate`) extracts those binaries and prepends them to `PATH`.
- Images: `#ContainerImage` is partially implemented: `cuenv build` lists image definitions and builds selected images with the local Docker CLI. Registry builds are pushed with Docker buildx; Dagger execution and downstream image output-reference resolution remain incomplete.
- Services: `cuenv up`, `ps`, `down`, `restart`, and `logs --follow` use persisted service session state; service-to-service dependencies are honored and selected service starts include their service deps; task dependencies in `services.*.dependsOn` run before service startup; image dependencies are recognized but selected image builds still fail fast until image execution backends are wired into service startup.
- Infrastructure (experimental): `#Infrastructure` is a partial proof of concept. `cuenv infrastructure` (`cuenv i`) plan, apply, destroy, state and unlock drive Terraform provider plugins (protocols 5 and 6) with lock-fenced, per-resource state in one Turso database keyed by CUE module path, project name and selected named environment (the no-flag identity is separate, and moving between them is refused rather than migrated). The schema checks every configuration (the top level and each `environments.NAME`) with CUE `error()` checks for providers, `dependsOn` references and the Turso URL, so a semantic error fails every command that evaluates the project; the Rust engine repeats the checks for the selected configuration. Apply follows a Terraform-style dependency graph validated at plan time; provider failures skip only dependent work, and replacements deleted but not recreated are reported. Provider processes withhold cuenv's secret-resolver credentials (or, with `providerEnvironment: "isolated"`, everything outside a small allowlist), and output, errors and logs are redacted. `cuenv infrastructure provider add` generates closed CUE types for a provider's block and managed resources from its own schema into `cue.mod/gen` and pins version, schema digest and archive hashes in `cuenv.lock`; plans refuse a provider whose schema or archive differs from the pin. Resources cannot reference each other's attributes; resource identity, data sources, imports, `state move`, provider version constraints and typed `path` providers are not supported yet.
- Tasks: groups, sequences, params, output refs, caching, `timeout`, `retry`, `continueOnError`, and group `maxConcurrency` are real. Hermetic host tasks use directory isolation by default, including when result caching is disabled, but absolute host paths and the network remain reachable; symlinked inputs are followed and staged as regular files (Bazel's source-file behaviour), and output symlinks are rejected. `timeout` is host-backend only (rejected on the dagger backend, which cannot tear down its container on elapse) and kills the task's whole process group; a timed-out attempt is never retried.
- CI and release: GitHub CI sync is the strongest path; Buildkite sync/export is partial; GitLab export/sync is schema-only and sync rejects it with a configuration error until an emitter exists; config-driven release backends are not complete.

Run `cuenv task ci.schema-docs-check` after changing schema, docs, prompts, examples, skills, or CLI surfaces.
