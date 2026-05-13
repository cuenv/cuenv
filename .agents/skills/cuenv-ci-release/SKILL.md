---
name: cuenv-ci-release
description: Use for cuenv CI schema, contributors, provider config, workflow sync, matrix tasks, artifacts, provider status, release config, changesets, changelog categories, targets, and release backends. Covers schema/ci.cue and schema/release.cue.
---

# CI and Release

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/ci.cue` for providers, pipelines, contributors, matrix tasks, artifacts, secrets, and action overrides.
- `schema/release.cue` for release targets, backends, git/tag config, versioning, changelogs, changesets, and package changes.
- `crates/cuenv/src/commands/sync` and `crates/cuenv/src/commands/release.rs` when behavior matters.

Status guardrails:

- CI workflow generation requires explicit `ci.providers`.
- Namespace cache support is `ci.provider.github.namespaceCache` plus `contributors.#NamespaceCache`; it emits `namespacelabs/nscloud-cache-action@v1` with `cache: nix`, intentionally does not install Nix, removes any restored `/nix/receipt.json` before Determinate Nix installs, and prunes the new receipt before the cache action saves state.
- Derived GitHub trigger paths are project-input based, but emitted as normalized repo-relative filters; nested inputs like `../flake.nix` must become `flake.nix`. Task inputs without glob metacharacters also emit a `path/**` companion entry so directory inputs trigger on descendants (mirroring `cuenv_core::affected::matches_pattern`). Inputs that escape the repository root are dropped and logged at `warn` level.
- `pipelines[*].continueOnError` (default `false`): when `true`, the per-project orchestrator does not abort on the first failure; dependents of the failing task become `task.skipped` events with a `DependencyFailed { dep }` reason while independent siblings keep running. A panic / `JoinError` is still fatal regardless of the flag — we don't reason about state after a panic. The pipeline still exits non-zero overall if any task failed; the flag controls scheduling, not success.
- Per-project task execution is parallel (bounded `JoinSet`, cap `CI_MAX_PARALLEL = 4` in `crates/ci/src/executor/orchestrator.rs`). Ready-but-capped tasks emit one `task.queued` event each — the priming loop no longer re-emits on every iteration.
- Cache eligibility skips emit `task.cache_skipped` events with a structured `CacheSkipReason` (`EmptyInputs`, `NonPathRef`, `NoResolvedInputs`, `RuntimeEnv`, `Disabled { reason }`, `NeverMode`, `HasherRootMismatch`, `HashFailed`).
- Use `cuenv sync ci` to generate workflows.
- Use `cuenv ci --export buildkite` for export-style CI output; GitLab export is not implemented.
- `--filter-matrix` and `--jobs` are accepted but not fully applied.
- Release schema is partial because CLI release commands do not fully load config from `env.cue`.

Adversarial prompts:

- "Generate GitLab CI." State schema exists but export/sync is not implemented.
- "Use `cuenv ci --generate github`." Correct to `cuenv sync ci`.
- "Configure Homebrew release in env.cue." Explain schema shape and current loading gap.
