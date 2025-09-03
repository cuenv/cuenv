# Phase 2 — CLI UX, API hygiene, docs, and tracing

Status: Planned
Owners: CLI, Core
Target window: 1 week

## Goals

- Make CLI outputs machine-friendly and consistent; define exit codes.
- Remove `#![allow(missing_docs)]`; fully document public API with examples.
- Introduce value-enums for flags; provide `#[must_use]` on important returns.
- Improve tracing spans around FFI and CLI surfaces.

## Scope (must)

- CLI
  - `env print` output formats as Clap `ValueEnum`: `json`, `env`, `simple`.
  - JSON output envelope: `{"status":"ok","data":{...}}` or `{"status":"error","error":{...}}`.
  - Stable exit codes:
    - 0: success
    - 2: CLI usage/config error
    - 3: CUE eval/FFI error
  - Ensure `--json` implies JSON envelope regardless of `--format`.
- Tracing
  - Add span fields: `operation_id`, `bridge_version`, sizes, durations.
  - Optional OTel feature flag only for exporting (wire in later phase).
- API hygiene
  - Remove `#![allow(missing_docs)]` across crates.
  - Add docs + runnable examples for public items (builders, evaluator, error types).
  - Mark important constructors and pure functions with `#[must_use]`.
  - Replace `?` in CLI crate with explicit `match` + user-friendly messages via `miette`.
- Type conversions
  - Implement `TryFrom<&Path>` for `PackageDir`, `TryFrom<&str>` for `PackageName`.
  - Replace ad-hoc constructors.

## Acceptance criteria

- `cuenv env print --json` always returns a JSON envelope suitable for piping.
- All public items documented; clippy pedantic passes without allows.
- CLI tests cover exit codes and formats.

## Commands

- devenv shell -- cargo clippy --workspace --all-targets --all-features -- -D warnings
- devenv shell -- cargo test --workspace
