# Phase 5 — Security hardening and diagnostics

Status: Planned
Owners: Security, Engine
Target window: 2+ weeks (iterative)

## Goals

- Introduce opt-in isolation for task execution (namespaces/landlock on Linux).
- Harden with sanitizers and Miri; extend diagnostics.

## Scope (must)

- Sanitizers/Miri
  - CI jobs (Linux only initially):
    - ASAN/UBSAN: `RUSTFLAGS="-Zsanitizer=address"`, nightly toolchain gate (or use cargo-asan).
    - TSAN for any multi-threaded components separate from FFI pointer lifetimes.
    - Miri for safe Rust code paths.
- Isolation (design + POC)
  - Capability model draft (capabilities flags by task).
  - POC landlock for filesystem read-only except whitelisted dirs.
- Diagnostics
  - Audit log format for critical operations; structured events with IDs.

## Acceptance criteria

- Sanitizer jobs pass on Linux.
- Draft capability spec merged; POC shows blocked file writes outside allowlist.

## Commands

- devenv shell -- cargo miri test
- devenv shell -- cargo test --workspace
