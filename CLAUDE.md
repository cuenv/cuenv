# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Rules

- Never allow clippy warnings, fix the root cause

## Project Overview

cuenv is a CUE-powered environment management and task orchestration system built in Rust with a Go FFI bridge for CUE evaluation. It provides type-safe environments with runtime secret resolution and parallel task execution with content-aware caching.

## Build Commands

**CRITICAL: Build operations take significant time. Never cancel these commands.**

All commands must be run through `cuenv` to ensure the nix flake environment is properly activated via hooks.

```bash
# Build entire workspace (90+ seconds)
cuenv task build

# Release build (45+ seconds)
cuenv task release.build

# Run all tests (45-60 seconds)
cuenv task test.unit

# Library tests only (30+ seconds, faster)
cuenv exec -- cargo test --lib --workspace

# Run clippy (15-20 seconds)
cuenv task lint

# Format code (treefmt handles Rust, Go, CUE, etc.)
cuenv task fmt.fix

# Check formatting (CI mode)
cuenv task fmt.check
```

### Using cuenv

The project uses `cuenv` with a `#NixFlake` hook that automatically runs `nix print-dev-env` before any command. This ensures the correct toolchain versions are always used. You don't need to manually enter a nix shell.

## Architecture

### Crate Structure

| Crate                             | Purpose                                                       |
| --------------------------------- | ------------------------------------------------------------- |
| **cuengine**                      | Go-Rust FFI bridge for CUE evaluation                         |
| **cuenv-core**                    | Shared types, task execution, caching, environment management |
| **cuenv**                         | CLI binary with TUI (clap + ratatui)                          |
| **cuenv-events**                  | Event system for UI frontends (CLI/JSON renderers)            |
| **cuenv-workspaces**              | Package manager workspace detection (npm, Cargo, pnpm, etc.)  |
| **cuenv-ci**                      | CI pipeline integration                                       |
| **cuenv-release**                 | Version management and publishing                             |
| **cuenv-dagger**                  | Optional containerized task execution backend                 |
| **cuenv-cubes**                   | CUE-based code generation                                     |
| **cuenv-ignore**                  | .gitignore/.dockerignore generation                           |
| **cuenv-codeowners**              | CODEOWNERS file generation                                    |
| **cuenv-github/gitlab/bitbucket** | VCS provider integrations                                     |

### Key Data Flow

1. CLI parses args → loads `env.cue`
2. CUE evaluated via Go FFI bridge (cuengine)
3. Deserialized to Rust types (`Project`, `Env`, `Tasks`)
4. Task graph built with petgraph (dependency resolution)
5. Tasks executed with hermetic isolation and caching
6. Events broadcast to UI renderers

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
cuenv exec -- cargo run -- env print --path examples/env-basic --package _examples
cuenv exec -- cargo run -- env print --path examples/env-basic --package _examples --output-format json
```

## Code Quality Checklist

Before committing:

```bash
cuenv task fmt.fix
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

## Troubleshooting

- **Build appears frozen**: Expected, builds take 90+ seconds initially
- **Go FFI tests fail**: Expected without CGO environment
- **cargo-audit/cargo-deny not found**: CI-only tools, skip locally
- **Rust edition errors**: Requires Rust 1.85.0+ (Edition 2024)
