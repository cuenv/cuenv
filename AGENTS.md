# CLAUDE.md

## Project Rules

- Never allow clippy warnings, fix the root cause.
- It doesn't matter if it's pre-existing, we fix issues; we don't swerve accountability.
- We use Nix for builds and checks, cuenv for orchestration and workflow validation, and choose the smallest check that proves the current change.
- `nix flake check -L --accept-flake-config` is the final review/merge gate and the escalation gate for broad-risk changes listed under Validation Strategy. Release-only version bumps from an already-green `main` do not rerun the local full flake gate; rely on current main CI plus version/lock consistency checks.
- Isolated draft commits may be committed and pushed after focused validation when no full-flake trigger applies. Do not request review, mark ready, merge, or cut a non-version-only release until the full root flake check has passed.
- Do not run a full root flake check for every isolated draft commit. Use focused validation for docs-only edits, simple test extractions or moves, behavior-preserving refactors, and one-crate test-only dependency changes when the focused gate proves the touched surface.
- Always update ./docs for all work.
- Every PR that changes `schema/**`, CLI behavior, sync providers, task execution, CI/release behavior, or examples must update `docs/design/specs/schema-coverage-matrix.md`.
- Every PR that changes prompts or agent guidance must update the affected docs and skills under `.agents/skills/`. Update the schema coverage matrix only when the change alters schema or CLI support status.
- Run `cuenv task ci.schema-docs-check` before requesting review when schema, docs, prompts, examples, skills, or CLI surfaces change.

## Project Overview

cuenv is a CUE-powered environment management and task orchestration system built in Rust with a Go FFI bridge for CUE evaluation. It provides type-safe environments with runtime secret resolution and parallel task execution with content-aware caching.

## Coding Standards

- No boolean function parameters and never more than 3 function parameters before adopting a options struct.

## Build Commands

**CRITICAL: Build operations take significant time. Never cancel these commands.**

Use Nix for builds and checks. Use `cuenv` for orchestration, workflow sync, formatting, and other workflows that are not naturally expressed as Nix builds or flake checks.

```bash
# Build the CLI package (45+ seconds)
nix build .#cuenv -L --accept-flake-config

# Run the final review/merge/broad-release gate, not the default per-commit check (90+ seconds)
nix flake check -L --accept-flake-config

# Run the nextest flake check only
nix build .#checks.x86_64-linux.cuenv-nextest -L --accept-flake-config

# Run clippy only
nix build .#checks.x86_64-linux.cuenv-clippy -L --accept-flake-config

# Format code
cuenv fmt --fix

# Sync generated CI workflows
cuenv sync ci

# Verify schema coverage docs and repo-local agent skills
cuenv task ci.schema-docs-check
```

## Validation Strategy

Default to the smallest validation set that proves the current change. Full flake checks are required evidence for review/merge readiness and broad-risk changes, not a default proof for every isolated draft commit. If the change does not match a required full-flake trigger below, do not run the full root flake check before a draft commit; commit and push the isolated draft change with the focused validation recorded in the PR.

Before starting a full root flake check, name the trigger that requires it. If no trigger applies, keep validation focused and record the focused gate instead.

Start with focused validation when the change is isolated and the focused gate directly covers the touched surface:

- Simple test extractions, mechanical refactors, test moves, or module splits with no behavior change: run `cuenv fmt --fix`, `git diff --check`, and the focused crate/module test such as `cuenv exec -- cargo test -p <crate> --lib <module>::tests`, or an app-local Nix test/clippy check when that is the local boundary. Add all-target clippy for the touched crate when the commit removes lint allowances or changes test-only dependencies.
- One-crate test-only Cargo manifest or lockfile changes, such as adding a dev-dependency used only by tests: run `cuenv fmt --fix`, `git diff --check`, the focused crate/module tests, and all-target clippy for that crate. Review `Cargo.lock` to confirm the delta is limited to the test dependency.
- Docs, prompts, examples, repo-local skills, or agent-guidance text such as `AGENTS.md`: run `cuenv task ci.schema-docs-check`; add `cuenv fmt --fix` only when formatting applies.
- Sync-provider changes that do not alter generated workflow contracts: run `cuenv sync ci --check` plus the focused tests for the touched provider.
- CLI behavior changes: run the focused Rust tests and at least one direct CLI smoke test for the changed command.

Do not run the full root flake check for:

- Exploratory review work while deciding what to change.
- Docs-only, prompt-only, repo-local skill-only, or agent-guidance-only edits, including `AGENTS.md` text.
- Simple mechanical test extractions, test moves, or behavior-preserving module splits while the PR remains draft.
- One-crate test-only dev-dependency changes while the PR remains draft, when the crate-local tests and clippy cover the touched test surface.
- Tiny scoped commits where a focused crate/module check proves the touched surface.

Full root flake check is required when any of these are true:

- Marking a PR ready for review, requesting review, merging, or cutting a release that is not a release-only version bump from an already-green `main`.
- Changing Nix expressions, flake outputs, build/check wiring, CI/release behavior, generated workflow contracts, or Cargo manifests/lockfiles that affect production dependencies, crate features, workspace membership, MSRV, published package metadata, or more than one crate.
- Changing cross-crate runtime behavior in evaluation, task execution, caching, secrets, hooks, events, sync, or provider boundaries.
- Changing schema or CLI support in a way that focused tests, direct CLI smoke tests, and schema docs checks cannot fully cover.
- A focused check fails in a way that could indicate broader workspace breakage.

Release-only version bumps are the exception to the release trigger when `HEAD`
matches the already-green `origin/main` commit. For those cuts, skip local test
execution and the full root flake check. Verify the target version is consistent
across `Cargo.toml` and `Cargo.lock`; run locked Cargo metadata; inspect that
the lockfile delta only updates workspace package versions; and run
`git diff --check` before committing, tagging, or publishing.

If a change does not match one of the required full-flake triggers, keep the check focused and record the focused validation in the PR. Keep draft commits isolated, push them, and update the PR with the focused validation that was actually run. If the PR is moving from draft to review, run the full flake check once after the focused commits have landed.

## Architecture

### Crate Structure

| Crate                             | Purpose                                                      |
| --------------------------------- | ------------------------------------------------------------ |
| **cuengine**                      | Go-Rust FFI bridge for CUE evaluation                        |
| **cuenv-core**                    | Shared types, task execution, environment management         |
| **cuenv-hooks**                   | Hook execution, state management, and approval system        |
| **cuenv-cache**                   | Content-addressed task caching infrastructure                |
| **cuenv-task-graph**              | Task graph DAG algorithms and dependency resolution          |
| **cuenv-task-discovery**          | Workspace scanning and TaskRef resolution                    |
| **cuenv**                         | CLI binary with TUI (clap + ratatui)                         |
| **cuenv-events**                  | Event system for UI frontends (CLI/JSON renderers)           |
| **cuenv-workspaces**              | Package manager workspace detection (npm, Cargo, pnpm, etc.) |
| **cuenv-ci**                      | CI pipeline integration                                      |
| **cuenv-release**                 | Version management and publishing                            |
| **cuenv-dagger**                  | Optional containerized task execution backend                |
| **cuenv-codegen**                 | CUE-based code generation                                    |
| **cuenv-ignore**                  | .gitignore/.dockerignore generation                          |
| **cuenv-codeowners**              | CODEOWNERS file generation                                   |
| **cuenv-github/gitlab/bitbucket** | VCS provider integrations                                    |

### Key Data Flow

1. CLI parses args -> loads `env.cue`.
2. CUE evaluated via Go FFI bridge (cuengine).
3. Deserialized to Rust types (`Project`, `Env`, `Tasks`).
4. Task graph built with cuenv-task-graph (petgraph-based dependency resolution).
5. Tasks executed with hermetic isolation and caching.
6. Events broadcast to UI renderers.

### Contributor Loop

Contributors are CUE-defined task injectors that modify the task DAG before execution. Both CLI (`cuenv task`) and CI (`cuenv ci`) use the same `ContributorEngine`.

**Data Flow:**

1. CUE evaluation produces Projects with Tasks (initial DAG).
2. `ContributorEngine` (in `cuenv-core`) applies contributors:
   - Evaluates `when` conditions (`workspaceMember`, `taskCommand`, etc.).
   - Injects contributor tasks with `cuenv:contributor:*` prefix.
   - Auto-associates user tasks with contributor setup tasks (by command).
   - Loops until no changes (stable DAG).
3. Final DAG passed to executor (CLI or CI).

**Contributor Task Naming:** `cuenv:contributor:{contributor}.{task}`

- Example: `cuenv:contributor:bun.workspace.install`

**Activation Conditions:**

- `workspaceMember: ["bun"]` - inject when project is bun workspace member.
- `taskCommand: ["bun", "bunx"]` - inject when tasks use these commands.
- `runtimeType: ["nix"]` - inject when project uses Nix runtime.

**Priority-Based Ordering:** (for CI stage assignment)

- 0-9: Bootstrap stage (Nix install)
- 10-49: Setup stage (cuenv, cachix, etc.)
- 50+: Success stage (post-build tasks)

**CI Providers:**

CI workflow generation requires explicit provider configuration. No workflows are emitted without `ci.providers`:

```cue
ci: {
    providers: ["github"]  // Required: explicit opt-in
    pipelines: { ... }
}
```

Per-pipeline `providers` completely overrides global (no merge).

**Key Files:**

- `crates/core/src/contributors.rs` - ContributorEngine implementation.
- `crates/core/src/ci.rs` - CI configuration and provider resolution.
- `contrib/contributors/*.cue` - CUE contributor definitions.
- `schema/ci.cue` - CI and contributor schema types.

### Critical Patterns

**Console Output**: Direct `println!`/`eprintln!` is forbidden via clippy lints. All output must go through `cuenv_events` macros to support multiple renderers (CLI, TUI, JSON).

**Task Execution**: Uses content-addressed caching. Cache keys derived from: input file hashes + command + environment + platform.

**Secrets**: Never stored on disk. Resolved at runtime from providers (1Password, AWS, Vault, etc.) and redacted from logs.

## Testing

```bash
# Specific crate tests
cuenv exec -- cargo test -p cuengine

# Integration tests
cuenv exec -- cargo test --test integration_tests

# FFI edge cases
cuenv exec -- cargo test --test ffi_edge_cases

# Benchmarks (60+ seconds)
cuenv task bench

# BDD tests (cucumber)
cuenv task test.bdd
```

### Validation After Changes

```bash
# Test CLI functionality
cuenv exec -- cargo run -- version
cuenv exec -- cargo run -- env print --path examples/env-basic --package examples
cuenv exec -- cargo run -- env print --path examples/env-basic --package examples --output json
```

## Code Quality Checklist

Before each isolated draft commit, run the focused checks that match the files touched. Include `git diff --check` and `cuenv fmt --fix` for code changes, app-local or crate-local checks for localized code, and `cuenv task ci.schema-docs-check` for schema, docs, prompts, examples, skills, or CLI surfaces. For one-crate test-only Cargo manifest or lockfile deltas, run the crate-local tests and all-target clippy instead of the full root flake check while the PR is still draft.

Before requesting review or marking a PR ready:

```bash
cuenv fmt --fix
cuenv sync ci --check
cuenv task ci.schema-docs-check
nix flake check -L --accept-flake-config
```

## Requirements

- **Rust**: MSRV 1.85.0 (Edition 2024)
- **Go**: 1.21+ with CGO enabled (for cuengine FFI)
- **Nix**: Optional but recommended for reproducible builds

## Key Files

- `Cargo.toml` - Workspace configuration
- `crates/cuengine/bridge.go` - Go FFI implementation
- `crates/cuengine/src/lib.rs` - Rust FFI wrapper
- `crates/core/src/manifest/` - Configuration types
- `crates/core/src/tasks/` - Task graph and execution
- `examples/env-basic/env.cue` - Test configuration
- `schema/` - CUE schema definitions
- `docs/design/specs/schema-coverage-matrix.md` - schema implementation status matrix
- `.agents/skills/` - repo-local agent skills that must stay aligned with schema status

## Releasing

- **No `v` prefix** on tags or release titles. Use `0.27.1`, never `v0.27.1`.
- Git tags must be annotated: `git tag -a 0.27.1 -m "message"`.
- Release commit message format: `release: 0.27.1`.
- Version lives in root `Cargo.toml` under `[workspace.package]`. All crates inherit via `version.workspace = true`. Update the workspace version and all `[workspace.dependencies]` version strings.
- Do not edit `cue.mod/module.cue` for release-only version bumps unless there is a separate CUE module metadata change. `cuenv sync` never writes `cue.mod/module.cue`; consumer projects update their schema dependency with `cue mod get`.
- Create a GitHub release with `gh release create <tag>` using the bare version as the title.

## Troubleshooting

- **Build appears frozen**: Expected, builds take 90+ seconds initially.
- **Go FFI tests fail**: Expected without CGO environment.
- **cargo-audit/cargo-deny not found**: CI-only tools, skip locally.
- **Rust edition errors**: Requires Rust 1.85.0+ (Edition 2024).
