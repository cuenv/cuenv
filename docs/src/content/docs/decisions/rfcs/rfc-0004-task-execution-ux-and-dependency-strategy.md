---
id: RFC-0004
title: Task Execution UX and Dependency Strategy
status: Draft
decision_date: 2025-09-25
approvers:
  - TBD
related_features:
  - features/cli/task.feature:1
  - features/cli/help.feature:1
---

## Summary

This RFC records the intended user experience and dependency resolution model for `cuenv task`. The existing implementation, centred on [crates/cuenv-cli/src/commands/task.rs](crates/cuenv-cli/src/commands/task.rs:10), establishes behaviour for listing tasks, executing individual or grouped tasks, and applying environments. Documenting these choices ensures the behaviour remains stable as we expand task orchestration.

## Problem Statement

Without explicit documentation of task graph semantics:

- Contributors cannot easily reason about when to use sequential versus graph execution.
- Users do not know what to expect when a task has dependencies, or how environment variables are applied.
- BDD scenarios (e.g. [features/cli/task.feature](features/cli/task.feature:1)) remain empty, missing valuable acceptance coverage.

We need a durable reference that captures intents, trade-offs, and consequences.

## Goals

1. Define how the CLI resolves tasks, including globbing, namespaces, and absence handling.
2. Clarify how dependencies are built into graphs and when sequential execution is enforced.
3. Document environment injection and output handling for `task` invocations.
4. Provide guidance on failure states, exit codes, and downstream notifications/events.

## Non-goals

- Redesigning the internal task executor (out of scope for the CLI layer).
- Replacing existing concurrency or caching strategies (covered in cuenv-core documentation).
- Defining CLI error formats (handled by RFC-0002).

## Proposed Approach

1. **Task Discovery and Listing**

   - When `cuenv task` is executed without arguments, list all available tasks sorted by manifest order, mirroring the logic in `execute_task` that returns `Available tasks`.
   - Support aliases (e.g. `cuenv t`) and ensure help text references them.

2. **Execution Semantics**

   - For `TaskDefinition::Single` without dependencies, execute directly and stream output if `capture_output` is enabled.
   - For singles with dependencies or groups, construct a `TaskGraph` via [TaskGraph::build_for_task](crates/cuenv-cli/src/commands/task.rs:92) and execute using `execute_graph`, preserving topological order.

3. **Environment Handling**

   - Build a task-specific environment using `Environment::build_for_task`, injecting base variables while respecting secret redaction.
   - Support future policy hooks via cuenv-core without altering CLI contracts.

4. **Failure Behaviour**

   - Abort on the first failing dependency, returning configuration errors with exit code 2 where applicable.
   - Provide structured output summarising success/failure, aligning with `format_task_results`.

5. **UX Enhancements**
   - Provide optional `--capture-output` and upcoming `--select` flag alignment with `features/cli/task.feature`.
   - Document event emission (via `Event::CommandProgress`) to enable TUI visualisations.

## Alternatives Considered

| Option                                   | Outcome                | Reason Rejected                                             |
| ---------------------------------------- | ---------------------- | ----------------------------------------------------------- |
| Always use graph execution               | Uniform implementation | Adds overhead for simple tasks, complicates debugging       |
| Require explicit dependency lists in CLI | Increased clarity      | Redundant with CUE manifest, contradicts declarative design |
| Force sequential execution for groups    | Predictable            | Prevents parallelism for independent subtasks               |

## Impact on Users

- Predictable task execution with clear failure messaging.
- Enhanced discoverability of tasks via consistent listing output.
- Scripts can trust exit codes and textual summaries.

## Migration Plan

- Publish this RFC and coordinate with cuenv-core team to ensure alignment.
- Fill [features/cli/task.feature](features/cli/task.feature:1) with scenarios covering listing, successful execution, dependency failure, and output capture.
- Ratify via ADR-0003 once behaviour is final.

## Features Alignment

| Feature Specification                                    | Coverage                              | Notes                                                    |
| -------------------------------------------------------- | ------------------------------------- | -------------------------------------------------------- |
| [features/cli/task.feature](features/cli/task.feature:1) | Pending scenarios defined by this RFC | Will cover listing, execution, dependency failure cases. |
| [features/cli/help.feature](features/cli/help.feature:1) | Pending                               | Help output must describe task behaviour and flags.      |

## Open Questions

1. Should task names support explicit namespaces (e.g. `build.backend`) in CLI flags?
2. How do we represent task convergence/deduplication across monorepo workspaces?
3. Do we need a `--dry-run` mode to preview task graphs?

## Related Artifacts

| Artifact                                                                                          | Purpose                                                |
| ------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| [crates/cuenv-cli/src/commands/task.rs](crates/cuenv-cli/src/commands/task.rs:10)                 | Primary CLI logic for task execution.                  |
| [adr-0003-task-graph-execution-strategy](/decisions/adrs/adr-0003-task-graph-execution-strategy/) | Ratified decision capturing final execution semantics. |
| [readme.md](readme.md:248)                                                                        | Public documentation for task commands.                |
| cuenv-core task executor docs (future)                                                            | Provide deeper technical details once published.       |
