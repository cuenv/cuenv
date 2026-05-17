---
title: Task Scripts
description: Keep env.cue readable with CUE-native embedded scripts
---

Use inline `script` for short task-local glue. Move longer scripts into files
and embed them with CUE's native `@embed` attribute.

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
