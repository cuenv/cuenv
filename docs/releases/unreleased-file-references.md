# cuenv (unreleased): file references

Draft release note for PR-B1 of the cross-project references plan
([#611](https://github.com/cuenv/cuenv/issues/611)). The version is chosen at
release time.

**Upgrade the cuenv binary before, or together with, `cue mod get
github.com/cuenv/cuenv`.** The schema now writes a `cuenvRef` field on every
task, group, service and image. A cuenv older than this release reads it on a
task group as a child task and fails on every project with a group.

## Files of another project, or of the module

A path in `inputs` now starts from one of three places, and the input says
which:

```cue
import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/acme/site/content:cuenv"
)

tasks: build: {
	command: "astro"
	args: ["build"]
	inputs: [
		"src/**",                                                           // this project
		content.#Files & {paths: ["articles/**"]},                          // another project, by import
		schema.#ModuleFiles & {paths: ["bun.lock", "services/shared/**"]},  // the module root
	]
}
```

The task that reads the files chooses them; the project that holds them
declares nothing. No task of the other project runs and no dependency is
added. A file reference is hashed for the cache key, staged where it lies under
the directory sandbox, compared with changed files by `cuenv ci` from its own
directory (so the #611 case, an article of `content` changing, now selects
`app`'s task under `cuenv ci --path app`), and turned into workflow path
filters by `cuenv sync ci`. Regenerate workflows with `cuenv sync ci`.

Replace `../` inputs with file references. A `..` input still works for now, and
`cuenv ci` still runs its task on every change; its warning now prints the
reference to write instead. `..` inputs will be rejected once cross-project
task references ship.

- **Every path must match at least one file** wherever a graph or affected
  detection reaches the task, `cuenv ci --all` and runs with nothing to compare
  included. A rename in the other project is an error for the task that reads
  it, never a silently smaller set of inputs. `cuenv sync ci` does not check it.
  A reference cannot be optional: a glob that is legitimately empty at times is
  an error for its readers then.
- Paths are relative to their anchor and stay inside it: no `..`, `.` or empty
  segment, no leading `/`, no surrounding whitespace.
- A file reference never follows a symbolic link or enters a nested repository
  (a submodule, a worktree): under a glob they are left out, and a path through
  one is an error.
- `#Files` is now a reserved definition on every project: a project that
  defines its own `#Files` fails to evaluate.
- A `#Base` has no `#Files`; files beside a base are module files.
- Never spread or iterate a reference's `paths`: that copies bare strings,
  which are paths of the consuming project. `["src/**", ...content.#Files &
{paths: [...]}]` is valid CUE and silently drops the reference.
- The project a reference names is found among what the reading project
  imports, directly or through another package, and nowhere else. Nothing else
  in the module is evaluated to find it, so a directory that does not evaluate
  no longer fails a task that does not use it. A reference fails its task when
  that project is not imported, is imported from a child directory that only
  inherits its `name` (import the project's own directory), declares another
  CUE package than `cuenv`, or comes from another CUE module.
- The CUE bridge evaluates an imported project once per importer, and binding a
  reference builds each imported package once more to read its `name`.
  Importing a very large project only to read its files costs evaluation time
  in every command that evaluates the importer.
- Root-relative inputs: an input that selects nothing under its project and
  matched a changed file only when read from the repository root still selects
  its task, and the warning now gives the `schema.#ModuleFiles` rewrite. An
  input that does select files under its project and whose name also matches a
  changed root file gets one note instead of nothing; both extra matches stop
  in a later release.

At language version `v0.18.0` a base is written `schema.#Base...`; embedding
`schema.#Base` there closes the package and rejects every child's `name`. Below
`v0.18.0` that spelling does not parse and plain `schema.#Base` works as
before.

## Every node says which project it belongs to

The schema stamps every task, group, sequence member, service and image with
its project (`cuenvRef: {kind, project, name}`), and every output reference
(`tasks.build.stdout`, an image's `ref`) with `cuenvProject`. A task of another
project reached through an import is therefore never taken for a task of yours
with the same name. Until cross-project task references through imports ship,
using one is an error, raised only where a graph, `cuenv ci` or `cuenv sync ci`
reaches the node that holds it:

- `dependsOn: [content.tasks.build]`, with a hint to use `content.#Files` for
  that project's files;
- another project's task in `ci.pipelines.<name>.tasks` or in a matrix entry's
  `task`, which fails only that pipeline;
- another project's output reference, a service's or image's dependency on
  another project, or another project's task in a service's `watch.rebuild`.

Breaking effects of the stamp:

- `cuenvRef` is a reserved field of a task group: no child can be called
  `cuenvRef`.
- Re-labelling an imported task (`tasks: articles: content.tasks.articles`)
  now conflicts on the project stamp. Put reusable task bodies in a definition
  of a package that is not a project.
- A dependency on another project's sequence whose members are all nested
  sequences carries no stamp and is still bound by its bare name.

A file reference without `paths` (`inputs: [content.#Files]`), which the CUE
evaluation cuenv uses lets through, no longer fails the whole project: it is an
error of its task only.

`cuenv ci` names the failing task without its file and line: its evaluation
does not collect source positions.

Cuetty's task list reads the new `cuenvRef` stamp on a task group as part of
the group, not as a task, and lists the tasks of projects on the new schema.
