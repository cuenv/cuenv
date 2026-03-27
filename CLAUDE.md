# CLAUDE.md

## Project Rules

- Never allow clippy warnings, fix the root cause.
- It doesn't matter if it's pre-existing, we fix issues; we don't swerve accountability.
- Nix owns builds and checks; cuenv is used for orchestration, sync, formatting, and non-build workflows.
- We cannot commit any code if `nix flake check -L --accept-flake-config` is not passing.

## Project Overview

cuenv is a CUE-powered environment management and task orchestration system built in Rust with a Go FFI bridge for CUE evaluation. It provides type-safe environments with runtime secret resolution and parallel task execution with content-aware caching.

## Coding Standards

- No boolean function parameters and never more than 3 function parameters before adopting a options struct

## Build & Validation Commands

**CRITICAL: Build operations take significant time. Never cancel these commands.**

Builds/checks are Nix-first. Prefer direct flake checks/builds for validation, and use cuenv for orchestration, sync, formatting, and non-build workflows.

```bash
# Aggregate flake checks (90+ seconds)
nix flake check -L --accept-flake-config

# Build default cuenv package from flake outputs
nix build .#cuenv -L --accept-flake-config

# Build individual check outputs for faster CI iteration
nix build .#checks.x86_64-linux.cuenv -L --accept-flake-config
nix build .#checks.x86_64-linux.cuenv-clippy -L --accept-flake-config

# Sync CI workflows (orchestration)
cuenv sync ci

# Format code (non-build workflow)
cuenv fmt --fix

# Check formatting (CI mode)
cuenv fmt
```

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

1. CLI parses args → loads `env.cue`
2. CUE evaluated via Go FFI bridge (cuengine)
3. Deserialized to Rust types (`Project`, `Env`, `Tasks`)
4. Task graph built with cuenv-task-graph (petgraph-based dependency resolution)
5. Tasks executed with hermetic isolation and caching
6. Events broadcast to UI renderers

### Contributor Loop

Contributors are CUE-defined task injectors that modify the task DAG before execution. Both CLI (`cuenv task`) and CI (`cuenv ci`) use the same `ContributorEngine`.

**Data Flow:**

1. CUE evaluation produces Projects with Tasks (initial DAG)
2. `ContributorEngine` (in `cuenv-core`) applies contributors:
   - Evaluates `when` conditions (`workspaceMember`, `taskCommand`, etc.)
   - Injects contributor tasks with `cuenv:contributor:*` prefix
   - Auto-associates user tasks with contributor setup tasks (by command)
   - Loops until no changes (stable DAG)
3. Final DAG passed to executor (CLI or CI)

**Contributor Task Naming:** `cuenv:contributor:{contributor}.{task}`

- Example: `cuenv:contributor:bun.workspace.install`

**Activation Conditions:**

- `workspaceMember: ["bun"]` - inject when project is bun workspace member
- `taskCommand: ["bun", "bunx"]` - inject when tasks use these commands
- `runtimeType: ["nix"]` - inject when project uses Nix runtime

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

- `crates/core/src/contributors.rs` - ContributorEngine implementation
- `crates/core/src/ci.rs` - CI configuration and provider resolution
- `contrib/contributors/*.cue` - CUE contributor definitions
- `schema/ci.cue` - CI and contributor schema types

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
cuenv exec -- cargo run -- env print --path examples/env-basic --package examples --output-format json
```

## Code Quality Checklist

Before committing:

```bash
cuenv fmt --fix
cuenv task lint
cuenv task test.unit
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

## Releasing

- **No `v` prefix** on tags or release titles. Use `0.27.1`, never `v0.27.1`.
- Git tags must be annotated: `git tag -a 0.27.1 -m "message"`
- Release commit message format: `release: 0.27.1`
- Version lives in root `Cargo.toml` under `[workspace.package]`. All crates inherit via `version.workspace = true`. Update the workspace version and all `[workspace.dependencies]` version strings.
- Create a GitHub release with `gh release create <tag>` using the bare version as the title.

## Troubleshooting

- **Build appears frozen**: Expected, builds take 90+ seconds initially
- **Go FFI tests fail**: Expected without CGO environment
- **cargo-audit/cargo-deny not found**: CI-only tools, skip locally
- **Rust edition errors**: Requires Rust 1.85.0+ (Edition 2024)
