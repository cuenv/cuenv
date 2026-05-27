---
name: cuenv-tasks-graph-cache
description: Use for cuenv task definitions, task groups, sequences, dependencies, params, inputs, outputs, captures, output refs, cache policy, hermetic execution, and task execution limitations. Covers schema/tasks.cue and schema/execution.cue.
---

# Tasks, Graph, Cache

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/tasks.cue` for `#Task`, `#TaskGroup`, `#TaskSequence`, params, inputs, outputs, cache, captures, and Dagger compatibility fields.
- `schema/execution.cue` for shared command and script shapes.
- `crates/core/src/tasks`, including `crates/core/src/tasks/params.rs` for task parameter models, `crates/core/src/tasks/retry.rs` for retry config, `crates/core/src/tasks/dagger.rs` for legacy task-level Dagger config, `crates/core/src/tasks/graph/build.rs` for task graph construction, `crates/core/src/tasks/graph/output_refs.rs` for output-ref dependency edges, `crates/core/src/tasks/graph/resolver.rs` for task path resolution, `crates/core/src/tasks/process.rs` for host process spawning/output/registry handling, `crates/cuenv/src/commands/handler/task_handler.rs` for CLI-to-request adaptation, and `crates/cuenv/src/commands/task` when behavior matters.
- `crates/core/src/tasks/graph_advanced_tests/` for advanced graph regression coverage grouped by cross-project references, synthetic hooks, workspace setup, cross-project hooks, error/label/build-for-task coverage, and scale/edge cases.
- `crates/cuenv/tests/cross_project_deps.rs` for CLI-level cross-project materialization coverage. Keep temporary repo/module setup and command execution on `Result` helpers instead of file-level unwrap/expect allowances.
- Task-related Cucumber BDD step definitions live under `crates/cuenv/tests/bdd_support/task_steps.rs`; keep new task behavior scenarios in that module instead of growing the root BDD runner.
- CLI task/exec integration coverage keeps the root runner in `crates/cuenv/tests/task_exec_integration.rs`; shared harness code and basic, examples, label-selection, and hermetic PATH scenarios live under `crates/cuenv/tests/task_exec_support/`. Keep basic and hermetic scenario setup on local `Result` helpers for fixture writes, UTF-8 path arguments, ordered-output checks, and PATH-line lookup instead of reintroducing file-level unwrap allowances.
- `crates/cuenv/tests/stress_tests.rs` for ignored large-graph CLI stress coverage. Keep graph mutation, dependency-edge wiring, parallel-group calculation, topological ordering, and task-position lookups on `Result` helpers instead of reintroducing file-level unwrap/expect or print suppressions.
- `crates/cuenv/tests/examples_dag_tests.rs` for example-project DAG coverage. Keep these tests on checked `Result` helpers for root discovery, manifest loading, graph construction, and CI/task lookups instead of reintroducing file-level unwrap/expect allowances.
- `crates/task-graph/src/graph.rs` for generic DAG primitives and ordering, `crates/task-graph/src/graph/analysis.rs` for affected-task and transitive-closure helpers, and `crates/task-graph/src/graph/resolver_build.rs` when group expansion or sequence ordering matters. Read-only graph data implements `TaskNodeData`; code paths that inject group-level or output-reference dependencies require `MutableTaskNodeData`.
- `crates/task-graph/benches/graph_benchmarks.rs` should propagate `GraphResult` through Criterion setup/measurement helpers instead of using benchmark-wide unwrap allowances.
- `crates/core/src/contributors/{model,context,engine,dag}.rs` and `crates/core/src/contributors/workspace.rs` when task contributor injection or auto-association affects the DAG.

Generation rules:

- Use explicit `schema.#Task`, `schema.#TaskGroup`, and `schema.#TaskSequence` in examples.
- Use CUE references in `dependsOn` instead of stale string examples when possible.
- Reusable or hidden task definitions preserve referenced task identity through CUE reference metadata; do not recommend JSON value matching as a fallback.
- Explain that output refs imply dependencies.
- Task output ref ownership is split under `crates/core/src/tasks/output_refs/`: `model.rs` parses placeholders, `extraction.rs` rewrites raw CUE JSON and host passthrough markers, and `resolver.rs` resolves completed task output before execution.
- CLI task execution phase ordering lives in `crates/cuenv/src/commands/task/execution.rs`; named/label selection resolution lives in `crates/cuenv/src/commands/task/execution/selection.rs`; task-list rendering and format selection lives in `crates/cuenv/src/commands/task/execution/listing.rs`; interactive picker handoff lives in `crates/cuenv/src/commands/task/execution/picker.rs`; task help/detail rendering lives in `crates/cuenv/src/commands/task/execution/help.rs`.
- CLI task result and tree/detail rendering should not rely on crate-wide `expect_used` allowances. Keep infallible `String` formatting local to the renderer and handle real conversions explicitly.
- Host process spawning, captured/inherited output, process registry updates, and Unix process-group setup live in `crates/core/src/tasks/process.rs`; keep that OS-specific `pre_exec` boundary out of task orchestration.
- Non-hermetic tasks default to the directory containing the `env.cue` file that defines the executable task body. Imported tasks keep their definition directory by default. Use `dir: "path"` for the legacy CUE-module-root-relative override, or `dir: {from: "definition" | "caller" | "module", path: "..."}` when imported/re-exported tasks need an explicit base.
- Call out limitations for `timeout`, `retry`, `continueOnError`, group `maxConcurrency`, and hermetic filesystem behavior unless matrix status changes.
- Treat task-level `dagger` as legacy; prefer runtime Dagger only when the matrix says it is appropriate.

Event surface (`cuenv-events`):

- `TaskEvent` covers `Started`, `CacheHit`, `CacheMiss`, `CacheSkipped { reason: CacheSkipReason }`, `Queued { queue_position }`, `Skipped { reason: SkipReason }`, `Retrying { attempt, max_attempts }`, `Output { stream, content }`, `Completed { success, exit_code, duration_ms }`, plus `GroupStarted` / `GroupCompleted` with counts.
- `Started` carries `task_kind: TaskKind` (`Task` / `Group` / `Sequence`) and `parent_group: Option<String>` for group correlation.
- `SystemEvent::EventGap { skipped }` is synthesised by `EventReceiver` when the broadcast bus lags so consumers (TUI, JSON renderer) can surface a gap indicator instead of silently dropping events. Public enums are `#[non_exhaustive]`.
- `cuenv-events::register_secret(...)` / `register_secrets(...)` enroll values; `redact(str)` rewrites them to `*_*`. The CLI renderer applies this automatically to anything routed through its output path; direct command output should use `println_redacted`, `print_redacted`, `eprintln_redacted`, or `eprint_redacted`, which write through explicit stdout/stderr handles instead of raw print macros.
- `CliRendererConfig` owns plain terminal color/verbosity settings; spinner UI toggles live in `CliSpinnerConfig` so task progress rendering remains isolated from non-spinner logs.
- CLI task-event rendering and spinner integration live in `crates/events/src/renderers/cli/task.rs`; service-event output lives in `crates/events/src/renderers/cli/service.rs`. Category renderers should use the CLI renderer stdout/stderr helpers instead of raw print macros. Keep config-free categories as associated functions instead of adding placeholder `self.config` reads.
- Spinner interleaved output should go through `SpinnerRenderer::print_above`, which uses `indicatif::MultiProgress::println` to preserve active progress-bar frames instead of writing directly to stderr. Keep indicatif `{...}` templates as named constants in `crates/events/src/renderers/spinner.rs`.
- Trace acceptance span assertion helpers in `crates/cuenv/tests/trace_testing/assertions.rs` should keep failure messages on ordinary `assert!` paths instead of reintroducing explicit `panic!` calls or local panic lint allowances.
- Trace acceptance tests in `crates/cuenv/tests/trace_acceptance.rs` should return `Result` from binary/root discovery, dry-run execution, DAG JSON parsing, and task lookup helpers instead of reintroducing file-level unwrap/expect or print suppressions.
- `ExecutorConfig::continue_on_error` makes `cuenv task` and library callers honour the same DAG semantics as `ci.pipelines[*].continueOnError` — dependents of a failing task get `task.skipped { DependencyFailed }` and independent siblings keep running. Panics / `JoinError` are still fatal.
- `cuenv-events` emits via a process-wide `EventSender` installed by `set_global_sender` at startup. The `emit_*!` macros and `cuenv_events::emit(category)` / `emit_with_source(source, category)` both go through it directly. `CuenvEventLayer` is retained as a public type so callers that emit via `tracing::info!(target: "cuenv::...")` still flow into the bus, but the in-tree macros bypass it. Its thin layer stays in `crates/events/src/layer.rs`; tracing field extraction, redaction, and typed event construction live in `crates/events/src/layer/visitor.rs`.
- The exported `emit_*!` macro definitions live in `crates/events/src/macros.rs`; crate-root hidden helpers remain available for `$crate` expansion and redacted print helpers.
- JSON event rendering should go through `JsonRenderer::render_to_writer` for testable JSON-lines output instead of raw print macros.

Adversarial prompts:

- "Run these tasks with maxConcurrency 2." State current executor limitations.
- "Retry a task three times." Check whether retry is implemented before recommending it.
- "Pass stdout from one task to another." Use task output refs and cite the example.
