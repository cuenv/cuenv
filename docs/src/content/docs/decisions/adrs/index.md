---
title: Architecture Decision Records Index
description: Catalog of ratified cuenv decisions
---

This section captures binding decisions derived from proposals and implementation experience.

## Accepted ADRs

- [ADR-0001: Hook Approval Gate for Environment Loading](/decisions/adrs/adr-0001-hook-approval-gate-for-environment-loading/)
- [ADR-0002: Background Hook Execution and Prompt-Handler Lifecycle](/decisions/adrs/adr-0002-background-hook-execution-with-shell-self-unload/)
- [ADR-0003: Task Graph Execution Strategy](/decisions/adrs/adr-0003-task-graph-execution-strategy/)
- [ADR-0004: Environment Export Filtering Policy](/decisions/adrs/adr-0004-environment-export-filtering-policy/)
- [ADR-0005: CLI Error Taxonomy and Exit Codes](/decisions/adrs/adr-0005-cli-error-taxonomy-and-exit-codes/)
- [ADR-0007: Cross-Project Task Dependencies](/decisions/adrs/adr-0007-cross-project-task-dependencies/)
- [ADR-0008: Hermetic, Input-Addressed Task Execution with Persistent Cache](/decisions/adrs/adr-0008-hermetic-task-execution-cache/)
- [ADR-0009: Single Internal Sync Registry](/decisions/adrs/adr-0009-single-internal-sync-registry/)
- [ADR-0010: Direct rio-vt Integration for Cuetty](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/)

## Superseded ADRs

- [ADR-0006: Library-First Architecture with Unified Provider System](/decisions/adrs/adr-0006-library-first-provider-system/) — superseded by [RFC-0006](/decisions/rfcs/rfc-0006-workspace-refactoring-roadmap/) and [ADR-0009](/decisions/adrs/adr-0009-single-internal-sync-registry/).

## Notes

- Each ADR references its originating RFC and the scenarios listed in `crates/cuenv/tests/bdd/features`.
- Superseded decisions remain available as historical context and point to
  their replacement.
