---
id: ADR-0003
title: Task Graph Execution Strategy
status: Accepted
decision_date: 2025-09-25
approvers:
  - Core Maintainers
related_features:
  - features/cli/task.feature:1
  - features/cli/help.feature:1
supersedes: []
superseded_by: []
---

## Context

`cuenv task` supports both simple single-task execution and dependency-aware orchestration. The CLI implementation in [crates/cuenv-cli/src/commands/task.rs](crates/cuenv-cli/src/commands/task.rs:92) builds task graphs when dependencies are present, while preserving lightweight execution for independent tasks. This ADR formalises the strategy so future changes remain consistent with user expectations and the architecture documented in [docs/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy.md](docs/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy.md:1).

## Decision

1. **Dual Execution Paths**

   - Tasks with no dependencies MUST execute directly via `TaskExecutor::execute_definition`.
   - Tasks with dependencies or group definitions MUST construct a `TaskGraph` and execute via `TaskExecutor::execute_graph`, respecting topological ordering.

2. **Listing Behaviour**

   - Invoking `cuenv task` without arguments MUST list available tasks in deterministic order.
   - Missing task names MUST produce configuration errors with guidance.

3. **Environment Injection**

   - The CLI MUST build a task-specific environment using `Environment::build_for_task`, applying base variables and policies before execution.

4. **Failure Handling**

   - Any failing task in the graph MUST abort subsequent executions and surface a configuration error containing the failing task name and exit code.

5. **Output Strategy**
   - When capture mode is enabled, the CLI MUST aggregate stdout/stderr per task and surface summaries per `format_task_results`.

## Consequences

- Users obtain predictable execution semantics for both simple and complex tasks.
- Tests and documentation can assert the dual-path behaviour.
- Future optimisations (parallelism, caching) must preserve these guarantees or supersede this ADR.

## Alignment with Features

| Feature Scenario                                                   | Impact                                                                        |
| ------------------------------------------------------------------ | ----------------------------------------------------------------------------- |
| [features/cli/task.feature](features/cli/task.feature:1) — Pending | Scenarios will validate listing, success, and failure behaviour defined here. |
| [features/cli/help.feature](features/cli/help.feature:1) — Pending | Help text must explain graph vs direct execution referencing this decision.   |

## Related Documents

- [docs/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy.md](docs/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy.md:1)
- [crates/cuenv-cli/src/commands/task.rs](crates/cuenv-cli/src/commands/task.rs:10)
- cuenv-core task graph implementation (future documentation)

## Status

Accepted — behaviour implemented in the CLI and relied upon by existing integration tests.

## Notes

Enhancements such as parallel execution tuning or dry-run previews MUST either comply with or supersede this ADR via a follow-up record.
