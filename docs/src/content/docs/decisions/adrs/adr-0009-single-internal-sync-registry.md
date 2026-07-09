---
id: ADR-0009
title: Single Internal Sync Registry
status: Accepted
decision_date: 2026-07-09
approvers:
  - Core Maintainers
related_features: []
supersedes:
  - ADR-0006
superseded_by: []
---

## Context

cuenv accumulated three sync abstractions:

1. the command-layer registry that actually dispatched `cuenv sync`;
2. a public builder and provider registry used only by its own tests and docs
   (already deleted by RFC-0006 because it never routed real CLI execution);
3. an unused sync trait in `cuenv-core`.

The public registry could build clap commands, but the static `SyncCommands`
enum still parsed the real CLI and the command-layer registry performed the
work. It also registered only a subset of the built-in providers. That made the
documented extension point misleading and allowed provider options and dispatch
behavior to drift. The surviving command-layer trait still split every provider
into `sync_path`/`sync_workspace` pairs, several of which read the process
working directory instead of the path the user selected.

## Decision

cuenv has one sync dispatch path under
`crates/cuenv/src/commands/sync/`:

- clap owns the stable, statically typed CLI surface;
- `SyncRequest` carries the normalized path, package, options, scope, and
  executor;
- each built-in provider implements one crate-private `SyncProvider::sync`
  method;
- `SyncRegistry` handles named and ordered multi-provider dispatch;
- the default registry contains every built-in provider: rules, VCS, lock,
  codegen, CI, and git hooks.

The unused `cuenv-core` sync trait is removed, completing the cleanup that
RFC-0006 started when it deleted the public builder, capability registry, and
duplicate provider implementations. Detection and rules-evaluation
helpers remain separate because they support the active providers rather than
forming another dispatch layer.

cuenv does not currently promise third-party sync plugins. A future extension
API must route actual command execution, define option ownership and stability,
and be accepted in a new ADR before it is exposed publicly.

## Consequences

### Positive

- There is one place to trace sync parsing, scope selection, and execution.
- Every built-in provider receives the same typed request.
- Multi-provider error aggregation lives in the registry instead of the
  command handler.
- Documentation no longer advertises an extension surface that the CLI ignores.

### Negative

- Rust consumers of the experimental `CuenvBuilder`, `ProviderRegistry`,
  `Provider`, and `SyncCapability` APIs must remove those references.
- Adding a built-in provider still requires updating the static CLI enum and
  the default registry.

### Neutral

- Existing `cuenv sync` commands and CUE schema are unchanged.
- This decision does not change runtime, secret, CI-provider, or release-provider
  dispatch.

## Validation

- Registry tests cover provider lookup and error aggregation.
- Sync scope integration tests cover path and workspace behavior.
- The schema/docs drift check rejects references to the removed public API in
  active guidance.
