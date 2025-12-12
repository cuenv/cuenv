---
id: RFC-0001
title: CLI Command Surface and Subcommand Hierarchy
status: Draft
decision_date: 2025-09-25
approvers:
  - TBD
related_features:
  - features/cli/env.feature:1
  - features/cli/help.feature:1
  - features/cli/task.feature:1
---

## Summary

This RFC defines the canonical command surface for the cuenv CLI. It captures the hierarchy, naming, default argument semantics, and discoverability strategies already prototyped in [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:211). The goal is to stabilise the structural expectations that other tooling (documentation, BDD scenarios, IDE integrations) depend on before the CLI transitions from alpha to beta.

## Problem Statement

The top-level command graph was introduced incrementally, guided by immediate implementation needs. Without a recorded rationale, contributors lack clarity about:

- Why specific subcommands (for example `env`, `task`, `exec`, `shell`, `allow`) are grouped as they are.
- Expectations around default flags (`--path`, `--package`, `--format`) and how global flags interact with subcommand-specific options.
- The relationship between discoverability commitments made in documentation and BDD features versus the actual implementation.

Left undocumented, future changes risk fragmenting the user experience and breaking scripted integrations.

## Goals

- Preserve a consistent, intuitive command hierarchy optimised for daily workflows.
- Codify defaults so help text, documentation, and tab-completion can rely on them.
- Provide clear guidance when new subcommands or aliases are introduced.
- Align feature specifications and tests with the documented structure.

## Non-goals

- Designing new commands that are not already in `main`.
- Describing execution semantics for background hooks or task graphs (covered separately in [rfc-0003-shell-integration-workflow-and-hook-lifecycle](/decisions/rfcs/rfc-0003-shell-integration-workflow-and-hook-lifecycle/) and [rfc-0004-task-execution-ux-and-dependency-strategy](/decisions/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy/)).
- Detailing error handling or output envelopes (handled in [rfc-0002-output-formatting-and-error-envelope-strategy](/decisions/rfcs/rfc-0002-output-formatting-and-error-envelope-strategy/)).

## Proposed Approach

1. **Command Taxonomy**

   - Retain the current top-level subcommands enumerated in [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:239).
   - Document the rationale for each grouping (`env` for environment inspection and hook lifecycle, `task` for orchestration, `exec` for ad-hoc commands, `shell` for integration, `allow` for approvals).

2. **Default Arguments**

   - Standardise `--path` defaulting to `.` and `--package` defaulting to `cuenv`.
   - Promote `--format` as a global flag with allowed values (`simple`, `env`, `json`), matching the `OutputFormat` value enum at [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:140).

3. **Discoverability Enhancements**

   - Ensure `--help` output surfaces subcommands, aliases, and examples consistent with documentation commitments in [readme.md](readme.md:242).
   - Provide canonical command usage snippets to be reused in docs and BDD scenarios.

4. **Stability Guarantees**

   - Define stability categories (stable, experimental, hidden) to guide future additions.
   - Define criteria to graduate an experimental command (minimum documentation, feature coverage, and telemetry).

5. **Versioning Strategy**
   - Track command additions/removals in release notes referencing this RFC to aid downstream consumers.

## Alternatives Considered

| Option                                                          | Outcome                          | Reason Rejected                                               |
| --------------------------------------------------------------- | -------------------------------- | ------------------------------------------------------------- |
| Keep the current implicit structure without documentation       | Collegial knowledge sharing only | Fragile for new contributors and downstream tooling           |
| Collapse `shell` subcommands into `env`                         | Fewer top-level commands         | Conflates configuration operations with integration bootstrap |
| Split `task` into multiple verbs (e.g. `task-run`, `task-list`) | Verb clarity                     | Increases binary size via Clap duplication and complicates UX |

## Impact on Users

- Shell scripts and CI pipelines can rely on stable flag defaults.
- CLI help text becomes a contract rather than an implementation detail.
- Feature authors can align scenario names with documented subcommands.

## Migration Plan

- Publish the RFC for review.
- Once accepted, update documentation and BDD feature placeholders (see alignment table) to reflect consensus.
- Track deviations via ADRs when behaviour is ratified (e.g. ADR-0003 for task execution).

## Features Alignment

| Feature Specification                                    | Coverage                                                 | Notes                                                    |
| -------------------------------------------------------- | -------------------------------------------------------- | -------------------------------------------------------- |
| [features/cli/help.feature](features/cli/help.feature:1) | `Scenario: Full CLI help surfaces subcommands` (pending) | This RFC defines the structure the scenario must assert. |
| [features/cli/env.feature](features/cli/env.feature:1)   | `Scenario: TBD` (pending)                                | To be updated to verify default path/package handling.   |
| [features/cli/task.feature](features/cli/task.feature:1) | `Scenario: Task listing respects hierarchy` (pending)    | Aligns aliases (`cuenv task`, `cuenv t`).                |

## Open Questions

1. Should experimental commands (e.g. `export`) be surfaced in help by default or hidden behind a feature flag?
2. Do we need per-subcommand telemetry or logging to justify future alterations?
3. How should we communicate deprecations (e.g. via `stderr` warnings or release notes only)?

## Related Artifacts

| Artifact                                                                                                  | Purpose                                                                           |
| --------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:211)                                            | Canonical definition of Clap command hierarchy.                                   |
| [readme.md](readme.md:242)                                                                                | Public CLI reference that must mirror the agreed structure.                       |
| [adr-0003-task-graph-execution-strategy](/decisions/adrs/adr-0003-task-graph-execution-strategy/)         | Downstream ADR addressing execution semantics that depend on this command layout. |
| [adr-0005-cli-error-taxonomy-and-exit-codes](/decisions/adrs/adr-0005-cli-error-taxonomy-and-exit-codes/) | Documents error handling expectations for commands defined here.                  |
