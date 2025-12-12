---
id: ADR-0001
title: Hook Approval Gate for Environment Loading
status: Accepted
decision_date: 2025-09-25
approvers:
  - Core Maintainers
related_features:
  - features/cli/hooks.feature:9
  - features/cli/env.feature:1
supersedes: []
superseded_by: []
---

## Context

`cuenv env load` initiates background hooks that configure development environments. The prototype in [crates/cuenv-cli/src/commands/hooks.rs](crates/cuenv-cli/src/commands/hooks.rs:69) introduced a configuration approval requirement to prevent unreviewed hook execution. Without a ratified decision the behaviour could regress, exposing users to malicious hook payloads or unexpected configuration drifts.

This ADR builds upon [rfc-0003-shell-integration-workflow-and-hook-lifecycle](/decisions/rfcs/rfc-0003-shell-integration-workflow-and-hook-lifecycle/) and stabilises the approval contract.

## Decision

1. **Mandatory Approval**

   - `cuenv env load` MUST verify configuration approval (via `ApprovalManager`) before executing hooks.
   - Approved configurations are keyed by canonical directory path and configuration hash. Unapproved configurations result in instructional guidance instead of execution.

2. **Feedback Messages**

   - When approval is missing or outdated, the CLI MUST return descriptive guidance, including truncated hashes and a summary derived from `ConfigSummary`.
   - Messages SHOULD direct users to `cuenv allow --path <dir>`.

3. **Silent Handling of Missing Files**

   - If no `env.cue` is present, `env load` MUST respond with a no-op message stating the absence of configuration instead of surfacing an error.

4. **Hashing Strategy**

   - Configuration hashes are computed from fully serialised manifests to capture effective changes.
   - Hash mismatches MUST invalidate prior approvals.

5. **Extensibility**
   - Approval storage defaults to the user-level file managed by `ApprovalManager::with_default_file`. Future backends (team storage) MUST preserve the contract documented here.

## Consequences

- Hooks will never run without an explicit `allow` step, improving security posture.
- Users receive deterministic messages when configuration drifts, aiding collaboration.
- CI pipelines that rely on automated approval must invoke `cuenv allow` prior to `env load`.

## Alignment with Features

| Feature Scenario                                                                                   | Impact                                                                              |
| -------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| [features/cli/hooks.feature](features/cli/hooks.feature:59) — Failed hooks do not load environment | Ensures approval gate prevents execution if configuration changes unapproved.       |
| [features/cli/hooks.feature](features/cli/hooks.feature:50) — Changing directories preserves state | Approval keying by canonical directory upholds this scenario.                       |
| [features/cli/env.feature](features/cli/env.feature:1) — Pending scenarios                         | Must include cases for approved vs. unapproved configurations referencing this ADR. |

## Related Documents

- [rfc-0003-shell-integration-workflow-and-hook-lifecycle](/decisions/rfcs/rfc-0003-shell-integration-workflow-and-hook-lifecycle/)
- [crates/cuenv-cli/src/commands/hooks.rs](crates/cuenv-cli/src/commands/hooks.rs:191)
- [adr-0002-background-hook-execution-with-shell-self-unload](/decisions/adrs/adr-0002-background-hook-execution-with-shell-self-unload/)

## Status

Accepted — implemented in the CLI and enforced by automated messaging.

## Notes

Subsequent work SHOULD explore team-level approval stores; such changes MUST produce a follow-up ADR that references this decision.
