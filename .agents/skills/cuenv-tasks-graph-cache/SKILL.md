---
name: cuenv-tasks-graph-cache
description: Use for cuenv task definitions, task groups, sequences, dependencies, params, inputs, outputs, captures, output refs, cache policy, hermetic execution, and task execution limitations. Covers schema/tasks.cue and schema/execution.cue.
---

# Tasks, Graph, Cache

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/tasks.cue` for `#Task`, `#TaskGroup`, `#TaskSequence`, params, inputs, outputs, cache, captures, and Dagger compatibility fields.
- `schema/execution.cue` for shared command and script shapes.
- `crates/core/src/tasks`, including `crates/core/src/tasks/params.rs` for task parameter models, `crates/core/src/tasks/retry.rs` for retry config, `crates/core/src/tasks/dagger.rs` for legacy task-level Dagger config, `crates/core/src/tasks/graph/resolver.rs` for task path resolution, `crates/core/src/tasks/process.rs` for host process spawning/output/registry handling, `crates/cuenv/src/commands/handler/task_handler.rs` for CLI-to-request adaptation, and `crates/cuenv/src/commands/task` when behavior matters.
- `crates/task-graph/src/graph.rs` for generic DAG primitives and ordering, `crates/task-graph/src/graph/analysis.rs` for affected-task and transitive-closure helpers, and `crates/task-graph/src/graph/resolver_build.rs` when group expansion or sequence ordering matters.
- `crates/core/src/contributors/{model,context,engine,dag}.rs` and `crates/core/src/contributors/workspace.rs` when task contributor injection or auto-association affects the DAG.

Generation rules:

- Use explicit `schema.#Task`, `schema.#TaskGroup`, and `schema.#TaskSequence` in examples.
- Use CUE references in `dependsOn` instead of stale string examples when possible.
- Reusable or hidden task definitions preserve referenced task identity through CUE reference metadata; do not recommend JSON value matching as a fallback.
- Explain that output refs imply dependencies.
- Task output ref ownership is split under `crates/core/src/tasks/output_refs/`: `model.rs` parses placeholders, `extraction.rs` rewrites raw CUE JSON and host passthrough markers, and `resolver.rs` resolves completed task output before execution.
- Non-hermetic tasks default to the directory containing the `env.cue` file that defines the executable task body. Imported tasks keep their definition directory by default. Use `dir: "path"` for the legacy CUE-module-root-relative override, or `dir: {from: "definition" | "caller" | "module", path: "..."}` when imported/re-exported tasks need an explicit base.
- Call out limitations for `timeout`, `retry`, `continueOnError`, group `maxConcurrency`, and hermetic filesystem behavior unless matrix status changes.
- Treat task-level `dagger` as legacy; prefer runtime Dagger only when the matrix says it is appropriate.

Event surface (`cuenv-events`):

- `TaskEvent` covers `Started`, `CacheHit`, `CacheMiss`, `CacheSkipped { reason: CacheSkipReason }`, `Queued { queue_position }`, `Skipped { reason: SkipReason }`, `Retrying { attempt, max_attempts }`, `Output { stream, content }`, `Completed { success, exit_code, duration_ms }`, plus `GroupStarted` / `GroupCompleted` with counts.
- `Started` carries `task_kind: TaskKind` (`Task` / `Group` / `Sequence`) and `parent_group: Option<String>` for group correlation.
- `SystemEvent::EventGap { skipped }` is synthesised by `EventReceiver` when the broadcast bus lags so consumers (TUI, JSON renderer) can surface a gap indicator instead of silently dropping events. Public enums are `#[non_exhaustive]`.
- `cuenv-events::register_secret(...)` / `register_secrets(...)` enroll values; `redact(str)` rewrites them to `*_*`. The CLI renderer applies this automatically to anything routed through its output path; direct command output should use `println_redacted`, `print_redacted`, `eprintln_redacted`, or `eprint_redacted` instead of raw print macros.
- `ExecutorConfig::continue_on_error` makes `cuenv task` and library callers honour the same DAG semantics as `ci.pipelines[*].continueOnError` — dependents of a failing task get `task.skipped { DependencyFailed }` and independent siblings keep running. Panics / `JoinError` are still fatal.
- `cuenv-events` emits via a process-wide `EventSender` installed by `set_global_sender` at startup. The `emit_*!` macros and `cuenv_events::emit(category)` / `emit_with_source(source, category)` both go through it directly. `CuenvEventLayer` is retained as a public type so callers that emit via `tracing::info!(target: "cuenv::...")` still flow into the bus, but the in-tree macros bypass it. Its thin layer stays in `crates/events/src/layer.rs`; tracing field extraction, redaction, and typed event construction live in `crates/events/src/layer/visitor.rs`.
- The exported `emit_*!` macro definitions live in `crates/events/src/macros.rs`; crate-root hidden helpers remain available for `$crate` expansion and redacted print helpers.

Adversarial prompts:

- "Run these tasks with maxConcurrency 2." State current executor limitations.
- "Retry a task three times." Check whether retry is implemented before recommending it.
- "Pass stdout from one task to another." Use task output refs and cite the example.
