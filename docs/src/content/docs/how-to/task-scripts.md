---
title: Task Scripts
description: Turn script directories into tasks, or embed scripts with CUE
---

Use inline `script` for short task-local glue. Move longer scripts into files
and embed them with CUE's native `@embed` attribute.

When a project already keeps a directory of scripts, list the directory in
`taskScripts` and every script in it becomes a task, with no CUE per script.

## Turn a Directory of Scripts into Tasks

```cue
schema.#Project & {
    name: "my-project"

    taskScripts: [{
        path:    "scripts"
        exclude: ["*.lib.sh"]           // helpers sourced by other scripts
        interpreters: "report.ts": "bun" // no shebang, so name the interpreter
    }]
}
```

With `scripts/release.sh`, `scripts/report.ts` and `scripts/lint.py`, the
project has the tasks `release`, `report` and `lint`:

```bash
cuenv task                       # lists them with their interpreter
cuenv task release -- 1.2.0 "notes with spaces"
```

How a file becomes a task:

- **Opt-in.** Nothing is scanned unless its directory is listed. Each entry is
  a directory relative to the project.
- **Which files.** Only files directly inside the directory, never in
  subdirectories. Dotfiles, names matching an `exclude` glob, and links or
  directories that resolve outside the project are skipped. A missing
  directory is skipped with a warning.
- **The interpreter.** An entry in `interpreters` wins, then the file's
  shebang (`#!/usr/bin/env bash`, `#!/usr/bin/env -S bun run`), then an
  extension only one interpreter uses: `.bash`, `.fish`, `.nu`, `.py`
  (`python3`). A file with neither a shebang nor such an extension, such as
  `README.md`, is not a task. A `.sh`, `.js` or `.ts` file without a shebang is
  listed, but running it fails as ambiguous: cuenv never guesses bash for
  `.sh` or Bun for JavaScript. Supported interpreters are `bash`, `sh`,
  `fish`, `nu`, `python`, `python3` (and versioned `python3.N` from a
  shebang) and `bun`; a script whose shebang names another program, such as
  `node`, fails with that name.
- **The name.** `prefix` followed by the file name without its extension:
  `prefix: "s-"` makes `scripts/release.sh` the task `s-release`. A name that
  clashes with a task declared in CUE, or with another script, fails when run
  and names both; the declared task keeps its definition.
- **How it runs.** The interpreter runs the script path from the project
  directory: `bash scripts/release.sh`. Arguments after the task name are
  passed through unchanged, after one leading `--`; there is no `${param}`
  interpolation.
- **Caching.** Discovered tasks run in the workspace and are never cached. A
  directory can opt into hermetic execution with the inputs, outputs and
  cache policy that needs; the script itself is always an input:

```cue
taskScripts: [{
    path:     "codegen"
    hermetic: true
    inputs:   ["schema/**"]
    outputs:  ["generated/**"]
}]
```

The interpreter must be on `PATH` or provided by the project runtime; a missing
one fails naming the program.

### In CI Pipelines

A discovered task has no CUE value to reference, so a pipeline names it with
`#TaskScriptRef`:

```cue
let _t = tasks

ci: pipelines: release: tasks: [
    {taskScript: "release"},
    _t.build,
]
```

`cuenv ci` and `cuenv sync ci` resolve it like any other task. A task's
`dependsOn` in CUE cannot name a discovered task.

See `examples/task-scripts` for a project with bash, Python, fish, Nushell and
Bun scripts.

## Embed a Script File

Directory layout:

```text
.
├── env.cue
└── scripts/
    └── release.sh
```

`env.cue`:

```cue
@extern(embed)
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    tasks: {
        release: schema.#Task & {
            script:      _ @embed(file=scripts/release.sh,type=text)
            scriptShell: "bash"
            inputs:      ["scripts/release.sh"]
        }
    }
}
```

`@embed` keeps the configuration CUE-native while removing large shell bodies
from `env.cue`. List the script file in `inputs` so hermetic execution and
task-result caching know that changing the file changes the task.

## Keep Small Scripts Inline

Inline scripts are still useful for short, readable commands:

```cue
tasks: {
    version: schema.#Task & {
        script: """
        git describe --tags --dirty
        """
        scriptShell: "bash"
    }
}
```

Once the script needs branching, helper functions, or more than a few lines,
prefer an embedded file.

## Choose the Shell

`scriptShell` accepts the supported task script interpreters:

```cue
tasks: {
    generate: schema.#Task & {
        script:      _ @embed(file=scripts/generate.py,type=text)
        scriptShell: "python"
        inputs:      ["scripts/generate.py", "schema/**"]
        outputs:     ["generated/**"]
    }
}
```

For shell scripts, tune strict-mode options explicitly when the defaults are not
right for the script:

```cue
tasks: {
    smoke: schema.#Task & {
        script:      _ @embed(file=scripts/smoke.sh,type=text)
        scriptShell: "bash"
        shellOptions: {
            errexit:  true
            nounset:  true
            pipefail: true
            xtrace:   false
        }
        inputs: ["scripts/smoke.sh"]
    }
}
```

## Cacheable Scripts

For tasks that should reuse results, declare both the script and the files it
reads:

```cue
tasks: {
    docs: schema.#Task & {
        script:      _ @embed(file=scripts/build-docs.sh,type=text)
        scriptShell: "bash"
        inputs: [
            "scripts/build-docs.sh",
            "docs/**",
            "package.json",
        ]
        outputs: ["dist/**"]
        cache: mode: "read-write"
    }
}
```

Use `hermetic: false` only for tasks that intentionally need the live checkout,
such as dev servers and interactive debugging tasks.

## See Also

- [Run tasks](/how-to/run-tasks/) - task execution, dependencies, and cache policy
- [CUE schema](/reference/cue-schema/) - task field reference
- [Schema status](/reference/schema/status/) - current task limitations
