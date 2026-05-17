---
title: CI
description: Generate and run CI pipelines with cuenv
---

cuenv CI runs the same task graph you define in `env.cue`. Workflow generation
requires an explicit provider list, so the repository only emits CI files when
you opt in.

## Define a Pipeline

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    let _t = tasks

    ci: {
        providers: ["github"]

        pipelines: {
            ci: {
                when: {
                    pullRequest: true
                    branch:      "main"
                }
                tasks: [_t.checks]
            }
        }
    }

    tasks: {
        checks: schema.#TaskGroup & {
            type: "group"

            lint: schema.#Task & {
                command: "cargo"
                args: ["clippy", "--workspace"]
                inputs: ["Cargo.toml", "Cargo.lock", "crates/**"]
            }

            test: schema.#Task & {
                command: "cargo"
                args: ["test", "--workspace"]
                inputs: ["Cargo.toml", "Cargo.lock", "crates/**"]
            }
        }
    }
}
```

Pipeline tasks are CUE references, not string names. This keeps task refs
type-checked and lets cuenv derive dependencies from the same graph used by
`cuenv task`.

## Generate Workflows

Generate GitHub Actions workflows:

```bash
cuenv sync ci
```

Check generated workflows in CI:

```bash
cuenv sync ci --check
```

Preview changes without writing:

```bash
cuenv sync ci --dry-run
```

Use `-A` in a workspace:

```bash
cuenv sync ci -A
```

## Run CI Locally

Run the default pipeline:

```bash
cuenv ci
```

Run a named pipeline:

```bash
cuenv ci --pipeline ci
```

Inspect what would run:

```bash
cuenv ci --pipeline ci --dry-run
```

## Provider Scope

GitHub workflow sync is the most complete provider path today. Provider-specific
configuration lives under `ci.provider.github`:

```cue
ci: {
    providers: ["github"]

    provider: github: {
        runner: "ubuntu-latest"
        permissions: {
            contents: "read"
        }
    }
}
```

Per-pipeline `providers` replaces the global provider list; it does not merge:

```cue
ci: {
    providers: ["github"]

    pipelines: {
        release: {
            providers: ["buildkite"]
            tasks: [_t.release]
        }
    }
}
```

Check [schema status](/reference/schema/status/) before relying on provider
surfaces outside the GitHub sync path.

## Secrets in CI

Use task-local secret refs when cuenv should resolve the value:

```cue
tasks: {
    deploy: schema.#Task & {
        command: "bash"
        args: ["scripts/deploy.sh"]
        inputs: ["scripts/deploy.sh"]
        env: {
            DEPLOY_TOKEN: schema.#OnePasswordRef & {
                ref: "op://Production/Deploy/token"
            }
        }
    }
}
```

Use `schema.#EnvPassthrough` when the CI provider already exposes the value:

```cue
env: {
    GH_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
}
```

## See Also

- [Run tasks](/how-to/run-tasks/) - define the task graph CI runs
- [Secrets](/how-to/secrets/) - runtime secret resolution
- [Schema status](/reference/schema/status/) - current CI provider coverage
