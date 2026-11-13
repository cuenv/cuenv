# cuenv (unreleased): task references through imports

Draft release note for PR-B2 and PR-C of the cross-project references plan
([#611](https://github.com/cuenv/cuenv/issues/611)), which ship together. The
version is chosen at release time. It follows the file references release; read
that note first.

**Breaking.** Upgrade the cuenv binary and the schema together:
`cue mod get github.com/cuenv/cuenv@<this version>`. `outputFiles` exists only
on the new schema, and on an older schema a dependency on an imported task is
bound by its stamp only when the stamp exists (the file references release has
it).

## Tasks of another project, through its import

```cue
import content "example.com/mono/content:cuenv"

tasks: build: {
	dependsOn: [content.tasks.articles]                    // a task of another project
	inputs: [content.tasks.articles.outputFiles, "src/**"] // its declared outputs, with the dependency
}
```

- `dependsOn: [content.tasks.articles]` depends on `content`'s task, never on a
  task of this project with the same name. It runs first, in `content`, with
  `content`'s own environment and cache identity, under `cuenv task`, `cuenv ci`
  and `cuenv up`.
- Every task now carries `outputFiles`, its declared `outputs` as an input of
  another task. Reading it also depends on the task, which must declare
  `outputs`. The files are staged where the producer writes them. Within a
  project, write `tasks.docs.build.outputFiles` rather than `{task:
  "docs.build"}`.
- A group can be depended on; a group cannot be an input yet (list its tasks'
  `outputFiles`). A whole sequence of another project, or a step of one, cannot
  be depended on. A task or group used as an input is a CUE error, and cuenv
  adds a hint to write `.outputFiles`.
- A project name is bound among what the referring project imports, never by
  listing the module: `cuenv task` no longer evaluates every directory of the
  module to follow a dependency, and a directory that fails to evaluate cannot
  fail a task that does not import it. A project that is not imported never
  binds, even when the module has one of that name.
- `cuenv sync ci` now accepts these dependencies. Trigger paths follow them into
  the producer (its inputs and file references, or its directory when it has no
  inputs), and on through the producer's own dependencies: its own tasks, and
  tasks of the projects it imports. A thin workflow runs them inside `cuenv ci`. A workflow that runs each
  task in a job of its own (expanded and release pipelines, matrix tasks) cannot
  run a task of another project: `cuenv sync ci` refuses to generate one whose
  tasks depend on one, and says to use `mode: "thin"`. Regenerate workflows.
- A task whose only inputs are other tasks' `outputFiles` is affected, and
  triggered, through those tasks, instead of on every change.
- Two projects that depend on each other's tasks are an import cycle: move what
  both need into a third project.
- `[..., ...content.tasks.x.outputFiles]` is valid CUE but drops the reference:
  never spread one.
- Re-labelling another project's task (`tasks: articles:
  content.tasks.articles`) conflicts on its project stamp; put reusable task
  bodies in a definition of a package that is not a project.

## Breaking changes

- **`..` inputs are rejected.** A string input with a `..` segment is a CUE
  error (`tasks.build._cuenvInputCheck."../content/articles/**": an input cannot
  leave the project directory`) and cuenv appends its path from the module root.
  Import the project that holds the files and write `<pkg>.#Files & {paths:
  [...]}`, or `<pkg>.tasks.<name>.outputFiles` for files a task builds; for
  files outside every project write `schema.#ModuleFiles & {paths: [...]}` with
  paths from the module root. Nothing above the module root can be named. On an
  older pinned schema cuenv refuses the task, with the same rewrite.
- **`#ProjectReference` is removed**, with the `{project, task, map}` input,
  the `#project:task:path` and `#project:task` input shorthands, and
  path-shaped project references (`../content`, `/content`). Import the project
  and read `<pkg>.tasks.<task>.outputFiles`, which also depends on the task.
  Nothing relocates another project's outputs: read them where they are built.
  An older pinned schema that still lets these through gets an error for the
  task that holds one, saying what to write; none is ever read as a task of this
  project.
- **`#TaskOutput` is no longer the default arm of `#Input`**, and is closed: an
  object with any other field is no task output.
- **A root file is no input of a project.** A changed file outside a project is
  no longer compared with that project's patterns from the repository root:
  - a pattern that selects nothing under its project (`shared/**` in `app`,
    with `shared/` only at the root) and matched only a root file now fails the
    `cuenv ci` run, saying to write `schema.#ModuleFiles & {paths:
    ["shared/**"]}` — the warning of the previous releases;
  - a root file named like an input of a correct project (`package.json`,
    `src/**`) no longer selects that project's tasks. The note of the previous
    releases is gone; add `schema.#ModuleFiles` if a task does read the root
    file.
- `outputFiles` is a reserved field of every task.
- Rust API: `Input::Project`, `ProjectReference`, `Task::task_ref`,
  `Task::from_task_ref`, `Task::iter_project_refs`,
  `TaskCacheConfig::project_roots`, `ExecutorConfig::sandbox_project_roots` and
  the path-shaped `ProjectSource` methods are removed; `ProjectSource` binds a
  project with `imported_project`. The CI IR task gains
  `external_dependencies`.

## Not across projects yet

Output references (`content.tasks.build.stdout`), another project's task in a
pipeline's `tasks` or a matrix entry's `task`, in a service's `watch.rebuild`,
or in a service's or image's `dependsOn`, and `outputFiles` on a group, remain
errors for the node that holds them.

## Limitations

- A dependency on an imported task is told apart from a local one by the
  schema's stamp. The source-location check the plan describes for older pinned
  schemas without stamps is not part of this release: on such a schema, upgrade
  it before depending on imported tasks, or an imported task can bind a
  same-named task of the consumer.
- Deep chains of `dependsOn` across imported projects evaluate in linear time
  under cuenv; stock `cue export` of such a chain can take minutes.
- The CUE bridge evaluates an imported project once per importer.
