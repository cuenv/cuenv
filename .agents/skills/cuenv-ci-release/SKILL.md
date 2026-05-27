---
name: cuenv-ci-release
description: Use for cuenv CI schema, contributors, provider config, workflow sync, matrix tasks, artifacts, provider status, release config, changesets, changelog categories, targets, and release backends. Covers schema/ci.cue and schema/release.cue.
---

# CI and Release

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/ci.cue` for providers, pipelines, contributors, matrix tasks, artifacts, secrets, and action overrides.
- `schema/release.cue` for release targets, backends, git/tag config, versioning, changelogs, changesets, and package changes.
- `crates/core/src/contributors/{model,context,engine,dag}.rs` for task contributor schema models, activation context, DAG injection, and verification helpers; `crates/core/src/contributors/workspace.rs` for built-in package-manager workspace contributors.
- `crates/release/src/manifest.rs` for the Cargo manifest entry point, `crates/release/src/manifest/packages.rs` for workspace package discovery/version/dependency reads, and `crates/release/src/manifest/updates.rs` for version writes.
- `crates/release/src/orchestrator.rs` for the release pipeline entry point, `crates/release/src/orchestrator/package.rs` for build/package artifact handling, and `crates/release/src/orchestrator/publish.rs` for backend publish orchestration.
- `crates/cuenv/src/commands/sync`, `crates/cuenv/src/commands/release.rs`, `crates/cuenv/src/commands/release/`, and `crates/cuenv/src/async_dispatch.rs` when CLI release or changeset behavior matters.
- `crates/cuenv/src/commands/sync/functions/github.rs` for GitHub workflow sync and non-matrix workflow emission, and `crates/cuenv/src/commands/sync/functions/github/matrix.rs` for matrix workflow expansion and artifact aggregation behavior.
- `crates/github/src/workflow/emitter.rs` for general GitHub Actions workflow emission, `crates/github/src/workflow/jobs.rs` for bootstrap/simple/matrix/artifact job construction, `crates/github/src/workflow/release.rs` for release workflow matrix/publish jobs, and `crates/github/src/workflow/emitter_tests/` for emitter regression coverage grouped by workflow, job, matrix/artifact, phase, working-directory, and trigger boundaries.
- `crates/ci/src/compiler/triggers.rs` for CI trigger assembly, normalized GitHub path filters, and workspace dependency trigger paths.
- `crates/ci/src/compiler/compiler_tests/` for compiler regression coverage grouped by compile, purity, trigger, contributor, path, dependency, and provider-detection boundaries.
- `crates/task-graph/src/graph/analysis.rs` for generic transitive closure helpers used by CI pipeline dependency expansion.
- `crates/ci/src/executor/orchestrator.rs` for pipeline scheduling, `crates/ci/src/executor/task_execution.rs` for per-task DAG execution and IR runner setup, `crates/ci/src/executor/reporting.rs` for reports/provider notification/annotations, `crates/ci/src/executor/hook_env.rs` for hook-backed CI environments, `crates/ci/src/executor/task_env.rs` for task env precedence, and `crates/ci/src/executor/tools.rs` for CI task tool activation.
- `crates/ci/src/diff.rs` for digest comparison and `crates/ci/src/diff/format.rs` for human-readable digest diff output.

Status guardrails:

- CI workflow generation requires explicit `ci.providers`.
- Secret setup contributors include `#OnePassword` for 1Password WASM setup and `#Infisical` for Infisical REST auth preflight; both activate through `secretsProvider`.
- Namespace cache support is `ci.provider.github.namespaceCache` plus `contributors.#NamespaceCache`; it emits `namespacelabs/nscloud-cache-action@v1` with `cache: nix` and `if: runner.os == 'Linux'`, intentionally does not install Nix, removes any restored `/nix/receipt.json` before Determinate Nix installs, and prunes the new receipt before the cache action saves state. macOS jobs skip the Namespace `/nix` cache action and continue with normal Nix installation.
- Derived GitHub trigger paths are project-input based, but emitted as normalized repo-relative filters; nested inputs like `../flake.nix` must become `flake.nix`. Task inputs without glob metacharacters also emit a `path/**` companion entry so directory inputs trigger on descendants (mirroring `cuenv_core::affected::matches_pattern`). Inputs that escape the repository root are dropped and logged at `warn` level.
- Runtime affected-task selection in `crates/ci/src/affected.rs` resolves local and external tasks through canonical `TaskIndex` instances. Core task indexing preserves the `#project:` separator, and CI cross-project refs split only on the first `:`, so nested refs like `#project:deploy:preview` keep `deploy:preview` as the task path.
- `pipelines[*].continueOnError` (default `false`): when `true`, the per-project orchestrator does not abort on the first failure; dependents of the failing task become `task.skipped` events with a `DependencyFailed { dep }` reason while independent siblings keep running. A panic / `JoinError` is still fatal regardless of the flag — we don't reason about state after a panic. The pipeline still exits non-zero overall if any task failed; the flag controls scheduling, not success.
- Per-project task DAG execution is parallel and bounded by `cuenv ci --jobs` via `crates/ci/src/executor/task_execution.rs`; the CLI default uses host parallelism. Ready-but-capped tasks emit one `task.queued` event each — the priming loop no longer re-emits on every iteration.
- CI pipeline report durations are clamped through checked non-negative conversions in `crates/ci/src/executor/orchestrator.rs`; display formatting should use duration helpers rather than lossy numeric casts. Live pipeline progress percentages in `crates/ci/src/report/progress.rs` should stay on bounded integer basis-point math before converting to display percentages.
- CI terminal reporter tests should read reporter state through named helpers with explicit failure messages instead of reintroducing module-level `unwrap_used` allowances.
- Mixed CLI integration tests in `crates/cuenv/tests/integration_tests.rs` cover version, env print, sync, and sync-ci command surfaces. Keep command execution behind `CliOutput`, git/CUE fixtures fallible, and failure diagnostics in assertion messages instead of reintroducing file-level unwrap/expect or raw print allowances.
- Sync-scope integration tests in `crates/cuenv/tests/sync_scope_rules.rs` should keep temp module setup, git initialization, CUE fixture writes, and cuenv command execution on `Result` helpers instead of reintroducing file-level unwrap/expect allowances.
- CI and workspace contributor integration tests should return `Result` and use named task/provider-hint helpers instead of reintroducing file-level `expect_used` allowances or raw skip output.
- CI garbage-collection default-policy tests in `crates/ci/src/gc.rs` should assert the runtime `GCConfig` bounds instead of reintroducing constant-only assertion suppressions.
- Cache eligibility skips emit `task.cache_skipped` events with a structured `CacheSkipReason` (`EmptyInputs`, `NonPathRef`, `NoResolvedInputs`, `RuntimeEnv`, `Disabled { reason }`, `NeverMode`, `HasherRootMismatch`, `HashFailed`).
- CI event CLI output is rendered in `crates/events/src/renderers/cli/ci.rs`; keep wording aligned there when CI event semantics change.
- The `cuenv-ci` crate root keeps only the temporary missing-docs allowance. Keep warning allowances scoped to the module that needs them instead of reintroducing crate-wide derive or parser suppressions.
- The `cuenv-release` crate root uses the crate's normal warning policy without broad derive-workaround allowances. Keep release warning suppressions scoped to the module that needs them.
- `crates/release/src/conventional.rs` owns conventional-commit parsing; keep gix commit walk ordering explicit and convert `git-conventional` components through their accessors instead of local interop lint suppressions.
- `crates/github/src/release.rs` owns GitHub release backend configuration and token environment parsing; keep tests on scoped `temp_env` overrides instead of unsafe process-wide environment mutation.
- Use `cuenv sync ci` to generate workflows.
- Use `cuenv ci --export buildkite` for export-style CI output; GitLab export is not implemented.
- `--jobs` is applied to local CI task DAG parallelism. `--filter-matrix` is rejected by the local runner until runtime matrix execution exists; provider-native matrix workflows still come from `cuenv sync ci`.
- Release schema is partial because CLI release commands do not fully load config from `env.cue`.

Adversarial prompts:

- "Generate GitLab CI." State schema exists but export/sync is not implemented.
- "Use `cuenv ci --generate github`." Correct to `cuenv sync ci`.
- "Configure Homebrew release in env.cue." Explain schema shape and current loading gap.
