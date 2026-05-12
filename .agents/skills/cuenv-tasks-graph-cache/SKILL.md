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

Adversarial prompts:

- "Run these tasks with maxConcurrency 2." State current executor limitations.
- "Retry a task three times." Check whether retry is implemented before recommending it.
- "Pass stdout from one task to another." Use task output refs and cite the example.

