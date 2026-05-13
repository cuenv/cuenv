---
name: cuenv-tasks-graph-cache
description: Use for cuenv task definitions, task groups, sequences, dependencies, params, inputs, outputs, captures, output refs, cache policy, hermetic execution, and task execution limitations. Covers schema/tasks.cue and schema/execution.cue.
---

# Tasks, Graph, Cache

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/tasks.cue` for `#Task`, `#TaskGroup`, `#TaskSequence`, params, inputs, outputs, cache, captures, and Dagger compatibility fields.
- `schema/execution.cue` for shared command and script shapes.
- `crates/core/src/tasks` and `crates/cuenv/src/commands/task` when behavior matters.

Generation rules:

- Use explicit `schema.#Task`, `schema.#TaskGroup`, and `schema.#TaskSequence` in examples.
- Use CUE references in `dependsOn` instead of stale string examples when possible.
- Explain that output refs imply dependencies.
- Call out limitations for `timeout`, `retry`, `continueOnError`, group `maxConcurrency`, and hermetic filesystem behavior unless matrix status changes.
- Treat task-level `dagger` as legacy; prefer runtime Dagger only when the matrix says it is appropriate.

Event surface (`cuenv-events`):

- `TaskEvent` covers `Started`, `CacheHit`, `CacheMiss`, `CacheSkipped { reason: CacheSkipReason }`, `Queued { queue_position }`, `Skipped { reason: SkipReason }`, `Retrying { attempt, max_attempts }`, `Output { stream, content }`, `Completed { success, exit_code, duration_ms }`, plus `GroupStarted` / `GroupCompleted` with counts.
- `Started` carries `task_kind: TaskKind` (`Task` / `Group` / `Sequence`) and `parent_group: Option<String>` for group correlation.
- `SystemEvent::EventGap { skipped }` is synthesised by `EventReceiver` when the broadcast bus lags so consumers (recorder, TUI, JSON renderer) can surface a gap indicator instead of silently dropping events. Public enums are `#[non_exhaustive]`.
- `cuenv-events::register_secret(...)` / `register_secrets(...)` enroll values; `redact(str)` rewrites them to `*_*` and is applied automatically by `EventRecorder::write_event` so JSONL recordings never contain plaintext secrets.
- `ExecutorConfig::continue_on_error` makes `cuenv task` and library callers honour the same DAG semantics as `ci.pipelines[*].continueOnError` — dependents of a failing task get `task.skipped { DependencyFailed }` and independent siblings keep running. Panics / `JoinError` are still fatal.
- `cuenv-events` exposes a direct-emit API: `set_global_sender(EventSender)` installs the process-wide bus, then `cuenv_events::emit(category)` / `emit_with_source(source, category)` publish a `CuenvEvent` without going through the `tracing::info!` macros. The existing `emit_*!` macros still route through `CuenvEventLayer` for now; the direct API is the seam for the planned migration away from tracing-as-transport.

Adversarial prompts:

- "Run these tasks with maxConcurrency 2." State current executor limitations.
- "Retry a task three times." Check whether retry is implemented before recommending it.
- "Pass stdout from one task to another." Use task output refs and cite the example.

