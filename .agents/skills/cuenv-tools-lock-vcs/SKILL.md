---
name: cuenv-tools-lock-vcs
description: Use for cuenv tools runtime, tool sources, platform overrides, extraction behavior, activation, lock sync, and VCS dependencies. Covers schema/tools.cue and schema/vcs.cue.
---

# Tools, Lock, VCS

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/tools.cue` for `#ToolsRuntime`, `#Tool`, overrides, source unions, extracts, and activation.
- `schema/vcs.cue` for VCS dependency definitions.
- `crates/cuenv/src/commands/tools.rs` and sync providers when behavior matters.

Status guardrails:

- Nix, GitHub, Rustup, and URL tool providers are registered.
- OCI is schema-visible in `#Source`, but the tool registry does not register an OCI provider.
- Use `cuenv sync vcs` for VCS dependencies.
- Use `cuenv tools activate` for lockfile activation metadata.
- `#VcsDependency.subdir` performs sparse-checkout of a single subtree (vendor-only). The lockfile records the subtree hash and re-syncs are deterministic.

Adversarial prompts:

- "Install a tool from an OCI image." State schema exists but provider support is missing.
- "Add platform-specific GitHub release assets." Use `#Override` and `#GitHubExtract`.
- "Sync VCS dependencies." Use `schema.#VcsDependency` and `cuenv sync vcs`.
- "Vendor only one directory of a remote repo (e.g. agent skills)." Use `subdir` with `vendor: true`; cuenv runs a sparse-checkout and lands only that subtree at `path`.

