---
id: ADR-0015
title: "CUE-Native Cross-Project References"
description: "Tasks and files of another project are reached through that project's CUE import; the value says which project it belongs to."
status: Accepted
decision_date: 2026-11-13
approvers:
  - Core Maintainers
related_features: []
supersedes:
  - ADR-0007
superseded_by: []
---

## Context

[ADR-0007](/decisions/adrs/adr-0007-cross-project-task-dependencies/) let a task
consume another project's outputs through a `{project, task, map}` input that
named the project by a string, its `name` or a path, and through `../` string
inputs. Neither form could be checked by CUE, both were resolved by scanning
the module, and `cuenv ci` could not compare `../` inputs with changed files:
a change to another project's file reported "No affected tasks"
([#611](https://github.com/cuenv/cuenv/issues/611)). The full design and its
reviews are in `docs/design/specs/2026-11-12-cross-project-references.md`.

## Decisions

- **References go through imports.** A task of another project is depended on
  as `dependsOn: [content.tasks.build]` and its declared outputs are read as
  `inputs: [content.tasks.build.outputFiles]`, which also adds the dependency.
  Files nobody builds are read as `content.#Files & {paths: [...]}`, files
  anywhere in the module as `schema.#ModuleFiles & {paths: [...]}`. The caller
  chooses the files; the other project declares nothing.
- **Identity travels in the value.** The schema stamps every task, group,
  service and image with its project (`cuenvRef`), every output reference with
  its project, and every task's `outputFiles` with its project and name. A
  stamp that names a project is authoritative: another project's task is never
  bound by its bare name to a task of the consumer.
- **A name binds among imports.** A project name is bound to its declaring
  directory among the packages the referring project imports, never by
  listing the module. A hand-written stamp of a project that is not imported
  binds nothing; nothing else is evaluated to bind a name.
- **Strings stay inside their project.** A string input with a `..` segment is
  rejected (schema and Rust), and a string is never read from the repository
  root.
- **Execution is unchanged from ADR-0007 where it was right.** A task of
  another project runs in its own project, with its own environment and cache
  identity, is part of the graph only, and its outputs are hashed by content.
  Its declared outputs are staged where it writes them; `map` relocation is
  kept for tasks of the same project only.
- **Removed.** `#ProjectReference`, the `#project:task:path` shorthand, and
  path-shaped project references. An older pinned schema that still emits them
  fails the task that holds one, with what to write instead.

## Consequences

- `cue export` checks every reference: a typo, a task used as an input, a
  project that is not imported are CUE errors or binding errors with the
  rewrite.
- Mutual dependencies between two projects are an import cycle and need a third
  project; a project cannot import one in a directory above it.
- `cuenv sync ci` follows a dependency on another project's task for trigger
  paths. A workflow that runs each task in a job of its own cannot run a task of
  another project and refuses one.
- Not yet across projects: output references (`.stdout`), pipeline entries,
  matrix tasks, service and image dependencies, `outputFiles` on groups.
