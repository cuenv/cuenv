---
id: ADR-0007
title: Evaluation of Ecdysis for Graceful Restarts
status: Deferred
decision_date: 2026-02-14
approvers:
  - Core Maintainers
related_features: []
supersedes: []
superseded_by: []
---

## Context

Issue: Investigate adopting [Ecdysis](https://blog.cloudflare.com/ecdysis-rust-graceful-restarts/) for graceful restarts.

cuenv's current long-lived process is the local `EventCoordinator` Unix socket server used for producer/consumer event fan-out. This process is intentionally lightweight, local-only, and can be restarted without user-visible downtime requirements today.

## Decision

Defer adoption of Ecdysis for now.

## Benefits (if adopted later)

1. **Zero-downtime coordinator upgrades**: listener/socket handoff would avoid short reconnect windows during restarts.
2. **Safer config/runtime reloads**: process replacement can preserve in-flight connections more cleanly than stop/start.
3. **Foundation for future daemonization**: if cuenv moves toward a persistent background agent, Ecdysis becomes more valuable.

## Why deferred

1. **Current value is low**: the coordinator is local and currently tolerant of short interruptions.
2. **Operational complexity**: graceful restart orchestration (signal handling, child supervision, readiness checks) adds maintenance burden.
3. **No immediate SLO pressure**: there is no production uptime requirement today that justifies the extra moving parts.

## Implementation plan (if/when this becomes worth it)

1. **Phase 1 — Spike in coordinator only**
   - Integrate Ecdysis in `crates/cuenv/src/coordinator/server.rs`.
   - Add restart trigger (signal or admin command) and verify listener handoff.
   - Add integration coverage for connected consumer reconnection behavior.
2. **Phase 2 — Hardening**
   - Add health/readiness state transitions before and after handoff.
   - Add structured event/trace instrumentation for restart lifecycle.
   - Define rollback path on failed child startup.
3. **Phase 3 — Adopt by default (conditional)**
   - Gate behind config/feature flag first.
   - Measure restart reliability and client impact in CI/e2e runs.
   - Remove flag only after demonstrated stability and clear user benefit.

## Revisit criteria

Re-open this decision if any of the following become true:

- cuenv introduces a always-on background daemon model,
- uptime/restart SLOs are added for coordinator availability, or
- live config reload becomes a hard product requirement.
