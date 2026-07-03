---
id: RFC-0006
title: Workspace Refactoring Roadmap — Leaf Types Crate and God-Crate Decomposition
status: Draft
decision_date: 2026-07-02
approvers:
  - TBD
related_features: []
---

## Summary

This RFC records a workspace-wide refactoring roadmap covering architecture, code quality, testing, and documentation. It supersedes the stalled root-level `ARCHITECTURE_REFACTOR.md` plan and the implementation approach of [ADR-0006](/decisions/adrs/adr-0006-library-first-provider-system/) (library-first provider system), whose parallel provider abstraction never became the runtime dispatch path.

The roadmap's central move is a new serde-DTO-only leaf crate, `cuenv-manifest`, which removes the coupling that twice stalled the previous decomposition effort: `Task`/`TaskNode` and manifest DTOs living inside `cuenv-core` made every downstream extraction a dependency cycle.

## Problem Statement

A workspace survey (33 crates, ~163k LOC) found that production code discipline is strong — near-zero unwrap/panic debt, consistent `thiserror` usage, no `anyhow`, only four inline lint allowances — but structural debt has accumulated:

1. **`cuenv-core` is a god-crate again.** The prior refactor reduced it from 22.2k to 15.3k LOC; it has regrown to ~29k LOC across 91 files, mixing task execution, tool activation, CI schema types, lockfile logic, the contributors engine, CUE discovery, secret-provider aggregation, and manifest DTOs. Its 770-line, 13-variant `Error` enum spans every domain. Thirteen crates depend on it.
2. **The prior plan stalled on a single root cause.** `ARCHITECTURE_REFACTOR.md` deferred Phase 5 (task index) and Phase 7 (CI types) because core's task/manifest types are not in a leaf crate — `crates/core/src/ci.rs` imports `crate::tasks::TaskNode` directly.
3. **Two parallel provider systems exist in the CLI.** The ADR-0006 "System A" (`Cuenv`/`CuenvBuilder`, `Provider`/`SyncCapability`, `ProviderRegistry`) duplicates the live "System B" (`SyncProvider`/`SyncRegistry` under `commands/sync/`). Runtime dispatch uses only System B; System A is exercised only by its own tests and doc examples (~2k LOC, with ~1k lines of near-duplicate provider implementations).
4. **Copy-paste across sibling crates.** GitLab/Bitbucket `codeowners.rs` have byte-identical `sync()`/`check()` bodies; three tool crates carry near-identical archive-extraction modules; `lockfile_entry_to_source` exists in three near-identical copies.
5. **Business logic in the CLI command tree.** ~2.1k LOC of VCS vendoring/materialization, ~2.1k LOC of release orchestration, and ~1.3k LOC of GitHub Actions generation live under `crates/cuenv/src/commands/` instead of their domain crates.
6. **Debt-hiding lint configuration.** The workspace globally allows `too_many_lines`, `too_many_arguments`, `cognitive_complexity`, and `redundant_clone` — exactly the lints that would flag the 37 production functions over 100 lines, 142 functions with ≥5 parameters, and unaudited clone hotspots.
7. **Test infrastructure gaps.** No shared test-support crate (fixture helpers duplicated across ≥15 sites), three coexisting test-placement conventions, seven test files over 800 lines, and thin coverage in `services` and the secret-backend crates.

## Decision

Execute the following phases in order. Each phase lands as isolated commits with focused validation per the repository's validation strategy; full `nix flake check` gates apply to new crates, multi-crate manifest changes, and cross-crate runtime changes.

### Phase 0 — Deletions and hygiene

- Delete provider System A (`crates/cuenv/src/{provider,registry,builder}.rs`, `providers/{ci,codegen,rules}.rs`, the `Cuenv` facade), preserving the live `detect_ci_provider` used by `commands/ci/`. ADR-0006 is marked superseded by this RFC; its library-first goal, if revived, will be built on the surviving `SyncRegistry` rather than a second abstraction.
- Remove the orphaned root `features/` BDD tree (the live tree is `crates/cuenv/tests/bdd/features/`), updating decision-record `related_features` references; remove stale audit docs; normalize ADR numbering.
- Centralize the 60+ dependency version re-pins into `[workspace.dependencies]`.
- Re-triage the RUSTSEC ignore list in `flake.nix`.

### Phase 1 — Duplication quick wins

- Lift `sync()`/`check()` into default methods on `cuenv_codeowners::provider::CodeOwnersProvider`, keyed on `output_path()` and `section_style()`.
- Consolidate archive extraction from `tools/github`, `tools/url`, and `tools/oci` into a shared crate under `crates/tools/`.
- Reduce `lockfile_entry_to_source` to a single canonical implementation.

### Phase 2 — `cuenv-manifest` leaf types crate

New crate `crates/manifest` containing serde DTOs only (deps: `serde`, `thiserror`, `cuenv-hooks`): manifest/config/environment DTOs, task types (`Task`, `TaskNode`, dependency/params/inputs/capture/cache-policy/retry types), CI DTOs from `core/src/ci.rs`, and lockfile schema types. `cuenv-core` re-exports during the transition. Concludes with decomposing core's monolithic `Error` enum into per-domain errors.

### Phase 3 — Drain the god-crate

- Task execution engine → `crates/task-exec`.
- Secret-backend decoupling: drop core's default features on `1password`/`aws`/`gcp`/`infisical` (and the transitive extism WASM runtime); registration moves to the CLI composition root via the existing `cuenv_secrets::SecretRegistry`.
- Tool activation/registry/provider → `crates/tools/runtime`.
- Remove transitional re-exports; core settles at ~8–10k LOC of genuinely shared logic.

### Phase 4 — Domain consolidation

- VCS materialization engine → `crates/vcs`.
- Release orchestration → `crates/release`.
- GitHub Actions workflow generation → `crates/github`.
- The CLI keeps thin adapters only.

### Phase 5 — Function-level quality

Decompose the worst oversized functions (`services` supervisor/controller, `dagger` execute, sync lock/vcs paths, hooks executor); convert genuine boolean flag parameters to two-variant enums; adopt the existing options-struct pattern for ≥5-parameter signatures.

### Phase 6 — Clippy ratchet

Crate-root `#![warn(...)]` attributes for the four debt lints land per crate as each is cleaned (crate-root attributes override the workspace-level `-A` flags); a final flip removes the workspace `allow`s and the per-crate attributes.

### Phase 7 — Test infrastructure

Shared `crates/test-support` dev-dependency crate with an explicit admission charter; standardize on sibling `_tests.rs` placement; split >800-line test files; coverage push for `services`, secret backends, `dagger`, `codeowners`, `vcs`; wire criterion benches as a non-gating check.

### Phase 8 — Documentation closure

Remove the `#![allow(missing_docs)]` escape hatch from `cuenv-ci`; add missing crate-level rustdoc; record the final decomposition as an ADR; delete `ARCHITECTURE_REFACTOR.md`; update the architecture overview and crate table.

## Consequences

- The deferred extractions from the prior plan (task index, CI types) become mechanical once Phase 2 lands.
- Breaking internal API changes are expected and acceptable; backwards compatibility was explicitly ruled out by the prior plan and that stance is retained.
- Thirteen crates re-import types from `cuenv-manifest`; the churn is bounded by transitional re-exports that are removed in Phase 3.
- ADR-0006's goal (external extensibility) is deferred, not abandoned; any future library surface builds on `SyncRegistry`.

## Status Tracking

Progress is tracked by phase on the implementation branch; each phase updates this RFC's status notes upon landing.

- Phase 0 (deletions and hygiene): landed — System A removed, orphaned
  BDD tree and stale docs deleted, ADRs renumbered, 60+ dependency
  re-pins centralized, RUSTSEC ignore list re-triaged (5 stale ignores
  dropped, survivors annotated by root cause).
- Phase 1 (duplication quick wins): landed — codeowners sync/check are
  default trait methods, `cuenv-tool-archive` consolidates the
  GitHub/URL extraction engines, lockfile-to-ToolSource conversion is
  a single method on `LockedToolPlatform`.
- Phase 2a-2d (`cuenv-manifest` leaf crate): landed — config,
  environment values, Secret, manifest DTOs, owners, task types,
  services/hook items, CI DTOs, Project, tool DTOs, and the lockfile
  schema all live in `crates/manifest`; core re-exports at the old
  paths and keeps resolution/execution behavior behind extension
  traits (`SecretExt`, `EnvValueExt`, `TaskCommandExt`,
  `Instance::to_project`).
- Remaining: Phase 2e (error decomposition) and Phases 3-8.
