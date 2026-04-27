---
title: Configuration Schema
description: Schema definitions and validation for cuenv configurations
---

This page documents the CUE schema definitions used in cuenv configurations. Import these from `github.com/cuenv/cuenv/schema` in your `env.cue` files.

## Root Schema

### #Project

The root configuration type that validates your entire `env.cue` file.

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Your configuration here
env: {...}
tasks: {...}
```

**Fields:**

| Field        | Type                          | Required | Description                          |
| ------------ | ----------------------------- | -------- | ------------------------------------ |
| `config`     | `#Config`                     | No       | Global configuration options         |
| `env`        | `#Env`                        | No       | Environment variable definitions     |
| `hooks`      | `#Hooks`                      | No       | Shell hooks for onEnter/onExit       |
| `name`       | `string`                      | Yes      | Project name (used by `#TaskRef`)    |
| `tasks`      | `{[string]: #Task}`           | No       | Task definitions                     |
| `services`   | `{[string]: #Service}`        | No       | Long-running supervised processes    |
| `images`     | `{[string]: #ContainerImage}` | No       | Container image build definitions    |
| `ci`         | `#CI`                         | No       | CI pipeline configuration            |
| `runtime`    | `#Runtime`                    | No       | Default runtime for tasks            |
| `codegen`    | `#Codegen`                    | No       | Code generation configuration        |
| `release`    | `#Release`                    | No       | Release management configuration     |
| `vcs`        | `{[#VcsDependencyName]: #VcsDependency}` | No       | Cuenv-managed Git dependencies       |

### #Base

Composable “base” configuration (no project-specific fields). This is useful for shared config in parent directories.

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {...}
  workspaces: {...}
}
```

## Configuration

### #Config

Global configuration options.

```cue
config: {
    outputFormat: "tui"  // or "spinner", "simple", "tree", "json"
}
```

**Fields:**

| Field          | Type     | Default | Description        |
| -------------- | -------- | ------- | ------------------ |
| `outputFormat` | `string` | -       | Task output format |

**Output Formats:**

| Format    | Description                |
| --------- | -------------------------- |
| `tui`     | Interactive terminal UI    |
| `spinner` | Simple spinner with status |
| `simple`  | Plain text output          |
| `tree`    | Tree-structured output     |
| `json`    | JSON output for scripting  |

## Environment

### #Env

Environment variable definitions with optional environment-specific overrides.

```cue
env: {
    NODE_ENV: "development"
    PORT: 3000
    DEBUG: true

    // Environment-specific overrides
    environment: {
        production: {
            NODE_ENV: "production"
            DEBUG: false
        }
    }
}
```

### #EnvironmentVariable

A single environment variable value. Can be:

```cue
env: {
    // Simple string
    NAME: "value"

    // Number (converted to string when exported)
    PORT: 3000

    // Boolean (converted to string when exported)
    DEBUG: true

    // Host environment passthrough for task execution
    GITHUB_ACTOR: schema.#EnvPassthrough
    TAG: schema.#EnvPassthrough & { name: "GITHUB_REF_NAME" }

    // Secret reference
    API_KEY: schema.#Secret & {
        command: "op"
        args: ["read", "op://vault/item/field"]
    }

    // Value with access policies
    DB_PASSWORD: {
        value: schema.#Secret & {...}
        policies: [{
            allowTasks: ["migrate"]
        }]
    }
}
```

`#EnvPassthrough` forwards a variable from the process running cuenv into the task environment.
Use it for CI-provided context such as GitHub Actions actor and ref values. When `name` is omitted,
cuenv reads the host variable with the same name as the env key.

Task-level `env` accepts the same value forms, including secret refs. For GitHub Actions tasks that
need to write outside the current repository, prefer a task-local `GH_TOKEN` secret because the
GitHub CLI reads `GH_TOKEN` before the repository-scoped `GITHUB_TOKEN`:

```cue
tasks: publish: schema.#Task & {
    env: GH_TOKEN: schema.#OnePasswordRef & {
        ref: "op://vault/github-token/password"
    }
    command: "gh"
    args: ["api", "-X", "PUT", "repos/org/other-repo/contents/file"]
}
```

Task-level environment entries are resolved at execution time. Tasks that define task-local `env`
entries are therefore excluded from task-result caching.
### #Environment

Environment variable naming constraint: must match `^[A-Z][A-Z0-9_]*$` (uppercase with underscores).

```cue
env: {
    VALID_NAME: "ok"      // Valid
    valid_name: "error"   // Invalid - must be uppercase
    123_NAME: "error"     // Invalid - must start with letter
}
```

## Tasks

### Task API v2 Overview

Tasks use **explicit type annotations** for clear semantics and compile-time validation:

- `#Task` - Single executable command or script
- `#TaskGroup` - Parallel execution (all children run concurrently)
- `#TaskSequence` - Sequential execution (steps run in order)

```cue
import "github.com/cuenv/cuenv/schema"

tasks: {
    // Single task with explicit type
    build: schema.#Task & {
        command: "cargo"
        args: ["build", "--release"]
    }

    // Parallel execution - all children run concurrently
    checks: schema.#TaskGroup & {
        type: "group"
        lint: schema.#Task & { command: "cargo", args: ["clippy"] }
        test: schema.#Task & { command: "cargo", args: ["test"] }
        fmt:  schema.#Task & { command: "cargo", args: ["fmt", "--check"] }
    }

    // Sequential execution - steps run in order
    deploy: schema.#TaskSequence & [
        schema.#Task & { command: "build" },
        schema.#Task & { command: "push" },
        schema.#Task & { command: "notify" },
    ]

    // Dependencies use CUE references (not strings)
    release: schema.#Task & {
        command: "release"
        dependsOn: [build, checks]  // References, not strings!
    }
}
```

### #TaskNode

Union of all task types - this is what gets validated:

```cue
#TaskNode: #Task | #TaskGroup | #TaskSequence
```

### #TaskOutputRef

A typed reference to another task's runtime output. Produced automatically when you reference `tasks.<name>.stdout`, `.stderr`, or `.exitCode` in CUE.

```cue
tasks: {
    tmpdir: schema.#Task & { command: "mktemp", args: ["-d"] }
    work: schema.#Task & {
        command: "echo"
        args: ["working in", tasks.tmpdir.stdout]  // → #TaskOutputRef
    }
}
```

**Fields:**

| Field            | Type                                     | Description                      |
| ---------------- | ---------------------------------------- | -------------------------------- |
| `cuenvOutputRef` | `true`                                   | Discriminator (always true)      |
| `cuenvTask`      | `string`                                 | Name of the referenced task      |
| `cuenvOutput`    | `"stdout" \| "stderr" \| "exitCode"`     | Which output field               |

You don't construct these manually — CUE generates them when you reference a task's `stdout`, `stderr`, or `exitCode` field. See [Task Output References](/how-to/run-tasks/#task-output-references) for usage.

### #Task

A single executable task (command or script-based).

```cue
tasks: {
    build: schema.#Task & {
        command: "cargo"
        args: ["build", "--release"]
        env: { RUST_LOG: "debug" }
        dependsOn: [lint, test]  // CUE references
        inputs: ["src/**/*.rs", "Cargo.toml"]
        outputs: ["target/release/myapp"]
        description: "Build the application"
    }

    // Script-based task
    setup: schema.#Task & {
        script: """
            echo "Setting up..."
            npm install
            npm run build
            """
        scriptShell: "bash"
        shellOptions: {
            errexit: true
            pipefail: true
        }
    }
}
```

**Fields:**

| Field            | Type                                              | Required | Description                              |
| ---------------- | ------------------------------------------------- | -------- | ---------------------------------------- |
| `command`        | `string`                                          | No*      | Command to execute                       |
| `args`           | `[...(string \| #TaskOutputRef)]`                 | No       | Command arguments                        |
| `script`         | `string`                                          | No*      | Multi-line script (alternative to cmd)   |
| `scriptShell`    | `#ScriptShell`                                    | No       | Shell for script execution (default: bash) |
| `shellOptions`   | `#ShellOptions`                                   | No       | POSIX shell options for `bash`/`zsh`, or `sh` with `pipefail: false` |
| `env`            | `{[string]: #EnvironmentVariable \| #TaskOutputRef}` | No   | Task-specific environment                |
| `dependsOn`      | `[...#TaskNode]`                                  | No       | Task dependencies (CUE references)       |
| `inputs`         | `[...#Input]`                                     | No       | Input file patterns for caching          |
| `outputs`        | `[...string]`                                     | No       | Output file patterns for caching         |
| `description`    | `string`                                          | No       | Human-readable description               |
| `hermetic`       | `bool`                                            | No       | Isolated execution (default: true)       |
| `timeout`        | `string`                                          | No       | Execution timeout (e.g., "30m")          |
| `continueOnError`| `bool`                                            | No       | Continue on failure (default: false)     |
| `stdout`         | `#TaskOutputRef`                                  | Auto     | Reference to this task's stdout          |
| `stderr`         | `#TaskOutputRef`                                  | Auto     | Reference to this task's stderr          |
| `exitCode`       | `#TaskOutputRef`                                  | Auto     | Reference to this task's exit code       |

*Either `command` or `script` should be provided.

**Script Shells:** `bash`, `sh`, `zsh`, `fish`, `nu`, `powershell`, `pwsh`, `python`, `node`, `ruby`, `perl`

When using `scriptShell: "sh"`, set `shellOptions.pipefail: false`. Plain `sh` does not reliably support `set -o pipefail`.

### #TaskGroup

Parallel execution - all child tasks run concurrently.

```cue
tasks: {
    checks: schema.#TaskGroup & {
        type: "group"  // Required discriminator
        maxConcurrency: 4  // Optional: limit parallel tasks
        description: "Run all checks in parallel"

        // Named children - all run concurrently
        lint: schema.#Task & { command: "cargo", args: ["clippy"] }
        test: schema.#Task & { command: "cargo", args: ["test"] }
        audit: schema.#Task & { command: "cargo", args: ["audit"] }
    }
}
```

**Fields:**

| Field            | Type             | Required | Description                        |
| ---------------- | ---------------- | -------- | ---------------------------------- |
| `type`           | `"group"`        | Yes      | Type discriminator                 |
| `dependsOn`      | `[...#TaskNode]` | No       | Dependencies (CUE references)      |
| `maxConcurrency` | `int`            | No       | Limit concurrent tasks (0 = unlimited) |
| `description`    | `string`         | No       | Human-readable description         |
| `{children}`     | `#TaskNode`      | No       | Named child tasks (any other field)|

### #TaskSequence

Sequential execution - tasks run in order, one after another.

```cue
tasks: {
    deploy: schema.#TaskSequence & [
        schema.#Task & { command: "build", description: "Build artifacts" },
        schema.#Task & { command: "test", description: "Run tests" },
        schema.#Task & { command: "push", description: "Push to registry" },
    ]
}
```

A sequence is simply an array of `#TaskNode` - the tasks execute in array order.

### #ExternalInput

Cross-project task dependencies (monorepo feature).

```cue
tasks: {
    build: {
        command: "build"
        externalInputs: [{
            project: "../shared-lib"
            task: "build"
            map: [{
                from: "dist/lib.js"
                to: "vendor/lib.js"
            }]
        }]
    }
}
```

**Fields:**

| Field     | Type            | Description                   |
| --------- | --------------- | ----------------------------- |
| `project` | `string`        | Path to external project      |
| `task`    | `string`        | Task name in external project |
| `map`     | `[...#Mapping]` | Output mappings               |

## Services

### #Service

Long-running supervised processes that live alongside tasks on a project. Services must reach a readiness state, are kept alive across the session, restart according to policy, and tear down on `cuenv down`.

```cue
services: {
    api: schema.#Service & {
        entrypoint: {
            command: "go"
            args:    ["run", "./cmd/api"]
        }
        readiness: { kind: "http", url: "http://localhost:8080/health" }
        restart: { mode: "onFailure" }
        watch: {
            paths: ["src/**/*.go"]
            rebuild: [tasks.build]
        }
        description: "API server"
    }
    db: schema.#Service & {
        entrypoint: {
            command: "postgres"
            args:    ["-D", "/var/lib/postgresql/data"]
        }
        readiness: { kind: "port", port: 5432 }
    }
    // A service whose entrypoint is a task reuses that task's command/script.
    worker: schema.#Service & {
        entrypoint: tasks.worker
        readiness:  { kind: "log", pattern: "worker ready" }
    }
    // Inline script form.
    migrator: schema.#Service & {
        entrypoint: {
            script: """
                set -eu
                psql -f /migrations/001_init.sql
                """
            scriptShell: "bash"
        }
        readiness: { kind: "log", pattern: "migration complete" }
    }
}
```

**Fields:**

| Field         | Type                              | Required | Description                                   |
| ------------- | --------------------------------- | -------- | --------------------------------------------- |
| `entrypoint`  | `#Task \| #Script \| #Command`    | Yes      | What the service runs (task, script, or command) |
| `env`         | `{[string]: #EnvironmentVariable}` | No       | Environment variables                         |
| `dir`         | `string`                          | No       | Working directory override                    |
| `dependsOn`   | `[...(#TaskNode \| #Service)]`     | No       | Dependencies on tasks or other services       |
| `labels`      | `[...string]`                     | No       | Labels for discovery                          |
| `description` | `string`                          | No       | Human-readable description                    |
| `runtime`     | `#Runtime`                        | No       | Runtime override                              |
| `readiness`   | `#Readiness`                      | No       | Readiness probe                               |
| `restart`     | `#RestartPolicy`                  | No       | Restart policy                                |
| `watch`       | `#Watch`                          | No       | File watcher for restart-on-change            |
| `logs`        | `#ServiceLogs`                    | No       | Log handling configuration                    |
| `shutdown`    | `#Shutdown`                       | No       | Shutdown behavior                             |
| `timeout`     | `string`                          | No       | Hard kill if startup exceeds this duration    |

### #Command and #Script

Reusable base shapes for invocation-style execution. `#Task` exposes the same fields directly; these base types are primarily consumed by `Service.entrypoint`.

```cue
#Command: {
    command: string
    args?:   [...(string | #TaskOutputRef)]
}

#Script: {
    script:        string
    scriptShell?:  #ScriptShell | *"bash"
    shellOptions?: #ShellOptions
}
```

`#Task` carries both `command`/`args` and `script`/`scriptShell`/`shellOptions` as optional fields; CUE evaluation rejects configurations that supply both a command and a script.

`Service.entrypoint` accepts any of `#Task`, `#Script`, or `#Command`. Using a task lets a service reuse an existing task definition (including its `args`, `env`, and working directory).

### #Readiness

Readiness probes determine when a service is ready to accept work. Discriminated by the `kind` field.

**Port probe** — TCP connection check:

```cue
readiness: {
    kind: "port"
    port: 8080
    host: "127.0.0.1"  // default
}
```

**HTTP probe** — HTTP request check:

```cue
readiness: {
    kind: "http"
    url: "http://localhost:8080/health"
    expectStatus: [200]  // default: 2xx
    method: "GET"        // default
}
```

**Log probe** — regex match on stdout/stderr:

```cue
readiness: {
    kind: "log"
    pattern: "Server started on port \\d+"
    source: "either"  // "stdout", "stderr", or "either" (default)
}
```

**Command probe** — exit 0 = ready:

```cue
readiness: {
    kind: "command"
    command: "pg_isready"
    args: ["-h", "localhost"]
}
```

**Delay probe** — fixed sleep (escape hatch):

```cue
readiness: {
    kind: "delay"
    delay: "5s"
}
```

All probes (except delay) support common timing fields:

| Field          | Type     | Default  | Description                          |
| -------------- | -------- | -------- | ------------------------------------ |
| `interval`     | `string` | `500ms`  | Time between probe attempts          |
| `timeout`      | `string` | `60s`    | Max time to reach ready              |
| `initialDelay` | `string` | `0s`     | Initial delay before first attempt   |

### #RestartPolicy

Controls how services restart after exit.

```cue
restart: {
    mode: "onFailure"  // never | onFailure | always | unlessStopped
    backoff: {
        initial: "1s"
        max:     "30s"
        factor:  2.0
    }
    maxRestarts: 5
    window: "60s"
}
```

| Field         | Type     | Default       | Description                              |
| ------------- | -------- | ------------- | ---------------------------------------- |
| `mode`        | `string` | `onFailure`   | Restart mode                             |
| `backoff`     | object   | -             | Exponential backoff config               |
| `maxRestarts` | `int`    | `5`           | Max restarts within sliding window       |
| `window`      | `string` | `60s`         | Sliding window for restart counting      |

### #Watch

File watcher that triggers service restarts on file changes.

```cue
watch: {
    paths: ["src/**/*.go", "go.mod"]
    ignore: ["*_test.go"]
    debounce: "200ms"
    on: "restart"
    rebuild: [tasks.build]
}
```

| Field      | Type            | Default   | Description                                  |
| ---------- | --------------- | --------- | -------------------------------------------- |
| `paths`    | `[...string]`   | -         | Glob patterns relative to project root       |
| `ignore`   | `[...string]`   | -         | Patterns to ignore (gitignore syntax)        |
| `debounce` | `string`        | `200ms`   | Debounce window for batched changes          |
| `on`       | `string`        | `restart` | Action on change (`restart`)                 |
| `rebuild`  | `[...#TaskNode]`| -         | Tasks to re-run before restart               |

### #Shutdown

Controls how services are stopped.

| Field     | Type     | Default   | Description                          |
| --------- | -------- | --------- | ------------------------------------ |
| `signal`  | `string` | `SIGTERM` | Signal to send (SIGTERM, SIGINT, SIGHUP, SIGQUIT) |
| `timeout` | `string` | `10s`     | Grace period before SIGKILL          |

### #ServiceLogs

Log handling configuration for services.

| Field     | Type     | Default        | Description                              |
| --------- | -------- | -------------- | ---------------------------------------- |
| `prefix`  | `string` | service name   | Stream prefix in multiplexed output      |
| `color`   | `string` | auto           | ANSI color hint (red, green, yellow, blue, magenta, cyan, white) |
| `persist` | `bool`   | `true`         | Persist logs to `.cuenv/run/<project>/logs/` |

## Container Images

### #ContainerImage

Declarative container image builds as first-class project artifacts. Images participate in the task DAG and produce output references (`.ref`, `.digest`) that downstream tasks can consume.

```cue
images: {
    api: schema.#ContainerImage & {
        context:    "."
        dockerfile: "Dockerfile"
        tags: ["latest", "v1.0.0"]
        registry: "ghcr.io/myorg"
        inputs: ["src/**", "Dockerfile"]
        description: "API server image"
    }
}
```

**Fields:**

| Field         | Type                                   | Required | Default        | Description                              |
| ------------- | -------------------------------------- | -------- | -------------- | ---------------------------------------- |
| `context`     | `string`                               | Yes      | -              | Build context directory                  |
| `dockerfile`  | `string`                               | No       | `"Dockerfile"` | Dockerfile path relative to context      |
| `buildArgs`   | `{[string]: string \| #ImageOutputRef}` | No       | -              | Build arguments                          |
| `target`      | `string`                               | No       | -              | Multi-stage build target                 |
| `tags`        | `[...string]`                          | No       | -              | Image tags                               |
| `registry`    | `string`                               | No       | -              | Registry to push to (omit for local)     |
| `repository`  | `string`                               | No       | -              | Repository name (defaults to image name) |
| `platform`    | `[...string]`                          | No       | -              | Target platforms for multi-arch builds   |
| `dependsOn`   | `[...#TaskNode \| #ContainerImage]`     | No       | -              | Dependencies on tasks or other images    |
| `labels`      | `[...string]`                          | No       | -              | Labels for discovery                     |
| `inputs`      | `[...#Input]`                          | No       | -              | Input files for cache key derivation     |
| `description` | `string`                               | No       | -              | Human-readable description               |

#### Output References

Images produce two output references resolved at runtime after the image is built:

| Reference | Description                                      |
| --------- | ------------------------------------------------ |
| `.ref`    | Full image reference (e.g., `ghcr.io/myorg/api@sha256:...`) |
| `.digest` | Content digest of the built image                |

Use these in downstream tasks:

```cue
tasks: {
    deploy: schema.#Task & {
        dependsOn: [images.api]
        env: IMAGE: images.api.ref
    }
}
```

#### Image Chains

Images can depend on other images for multi-layer builds:

```cue
images: {
    base: schema.#ContainerImage & {
        context: "docker/base"
    }
    api: schema.#ContainerImage & {
        context: "."
        dependsOn: [images.base]
        buildArgs: BASE_IMAGE: images.base.ref
    }
}
```

#### CLI

```bash
cuenv build              # List all images
cuenv build api          # Build specific image
cuenv build --label ci   # Build images matching label
```

## Hooks

### #Hooks

Shell hooks executed on directory entry/exit.

```cue
hooks: {
    onEnter: {
        setup: {
            command: "echo"
            args: ["Entering project"]
        }
    }
    onExit: {
        cleanup: {
            command: "echo"
            args: ["Leaving project"]
        }
    }
}
```

### #ExecHook

A single hook definition.

```cue
hooks: {
    onEnter: {
        nix: schema.#NixFlake

        custom: {
            order: 50
            propagate: true
            command: "setup.sh"
            args: ["--dev"]
            dir: "."
            inputs: ["setup.sh"]
            source: false
        }
    }
}
```

**Fields:**

| Field       | Type          | Default  | Description                       |
| ----------- | ------------- | -------- | --------------------------------- |
| `command`   | `string`      | required | Command to execute                |
| `args`      | `[...string]` | `[]`     | Command arguments                 |
| `order`     | `int`         | 100      | Execution order (lower = earlier) |
| `propagate` | `bool`        | false    | Export variables to children      |
| `dir`       | `string`      | "."      | Working directory                 |
| `inputs`    | `[...string]` | `[]`     | Input files for cache tracking    |
| `source`    | `bool`        | false    | Source output as shell script     |

### #NixFlake

Built-in hook for loading Nix flake environments.

```cue
hooks: {
    onEnter: {
        nix: schema.#NixFlake
    }
}
```

**Definition:**

```cue
#NixFlake: #ExecHook & {
    order:     10
    propagate: false
    command:   "nix"
    args:      ["print-dev-env"]
    source:    true
    inputs:    ["flake.nix", "flake.lock"]
}
```

## Shells

### #Shell

Shell configuration for task execution.

```cue
shell: {
    command: "bash"
    flag: "-c"
}
```

### Built-in Shells

| Type          | Command    | Flag     |
| ------------- | ---------- | -------- |
| `#Bash`       | bash       | -c       |
| `#Zsh`        | zsh        | -c       |
| `#Fish`       | fish       | -c       |
| `#Sh`         | sh         | -c       |
| `#Cmd`        | cmd        | /C       |
| `#PowerShell` | powershell | -Command |

**Usage:**

```cue
tasks: {
    build: {
        shell: schema.#Bash
        command: "echo"
        args: ["Building..."]
    }
}
```

## Secrets

### #Secret

Base secret type with exec-based resolution.

```cue
env: {
    MY_SECRET: schema.#Secret & {
        resolver: "exec"
        command: "echo"
        args: ["secret-value"]
    }
}
```

**Fields:**

| Field      | Type          | Description                |
| ---------- | ------------- | -------------------------- |
| `resolver` | `"exec"`      | Always "exec"              |
| `command`  | `string`      | Command to retrieve secret |
| `args`     | `[...string]` | Command arguments          |

### #OnePasswordRef

1Password secret reference.

```cue
env: {
    API_KEY: schema.#OnePasswordRef & {
        ref: "op://vault/item/field"
    }
}
```

**Fields:**

| Field | Type     | Description             |
| ----- | -------- | ----------------------- |
| `ref` | `string` | 1Password reference URI |

### #GcpSecret

Google Cloud Secret Manager reference.

```cue
env: {
    DB_PASSWORD: schema.#GcpSecret & {
        project: "my-project"
        secret: "db-password"
        version: "latest"  // default
    }
}
```

**Fields:**

| Field     | Type     | Default  | Description    |
| --------- | -------- | -------- | -------------- |
| `project` | `string` | required | GCP project ID |
| `secret`  | `string` | required | Secret name    |
| `version` | `string` | "latest" | Secret version |

## Policies

### #Policy

Access control policy for environment variables.

```cue
env: {
    SENSITIVE_KEY: {
        value: schema.#Secret & {...}
        policies: [{
            allowTasks: ["deploy", "release"]
            allowExec: ["kubectl", "helm"]
        }]
    }
}
```

**Fields:**

| Field        | Type          | Description                         |
| ------------ | ------------- | ----------------------------------- |
| `allowTasks` | `[...string]` | Tasks that can access this variable |
| `allowExec`  | `[...string]` | Exec commands that can access       |

## Workspaces

### #Workspaces

Workspace configuration for monorepos.

```cue
workspaces: {
    "packages/core": {
        enabled: true
        package_manager: "pnpm"
        root: "packages/core"
    }
}
```

### #WorkspaceConfig

**Fields:**

| Field             | Type     | Default | Description              |
| ----------------- | -------- | ------- | ------------------------ |
| `enabled`         | `bool`   | true    | Enable this workspace    |
| `package_manager` | `string` | -       | Package manager type     |
| `root`            | `string` | -       | Workspace root directory |

**Package Managers:**

- `npm`
- `pnpm`
- `yarn`
- `yarn-classic`
- `bun`
- `cargo`

## VCS Dependencies

Top-level Git dependencies managed by `cuenv sync vcs`.

```cue
vcs: {
    mylib: {
        url:       "https://github.com/example/mylib.git"
        reference: "main"
        vendor:    true
        path:      "vendor/mylib"
    }
}
```

**Fields:**

VCS dependency names must match `#VcsDependencyName`: ASCII letters, digits, `_`, `-`, and `.`, with no leading `.` and no `..`.

| Field       | Type     | Required | Description                                      |
| ----------- | -------- | -------- | ------------------------------------------------ |
| `url`       | `string` | Yes      | Git repository URL                               |
| `reference` | `string` | No       | Branch, tag, or commit-ish. Default: `HEAD`      |
| `vendor`    | `bool`   | Yes      | Track a snapshot (`true`) or local checkout      |
| `path`      | `string` | Yes      | Repository-relative materialization path         |

## Runtimes

### #ToolsRuntime

Tool management runtime for hermetic, reproducible CLI tools.

```cue
import "github.com/cuenv/cuenv/schema"

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    tools: {
        jq: "1.7.1"
        yq: "4.44.6"
    }
}
```

**Fields:**

| Field       | Type                        | Required | Description                                     |
| ----------- | --------------------------- | -------- | ----------------------------------------------- |
| `platforms` | `[...#Platform]`            | Yes      | Platforms to resolve and lock                   |
| `tools`     | `{[string]: string\|#Tool}` | Yes      | Tool specifications                             |
| `flakes`    | `{[string]: string}`        | No       | Named Nix flake references                      |
| `cacheDir`  | `string`                    | No       | Cache directory (default: ~/.cache/cuenv/tools) |

**Platforms:** `darwin-arm64`, `darwin-x86_64`, `linux-x86_64`, `linux-arm64`

### #Tool

Full tool specification with source and platform overrides.

```cue
tools: {
    bun: {
        version: "1.3.5"
        source: schema.#Homebrew
        overrides: [
            {os: "linux", source: schema.#Oci & {image: "oven/bun:1.3.5", path: "/usr/local/bin/bun"}}
        ]
    }
}
```

**Fields:**

| Field       | Type             | Required | Description                          |
| ----------- | ---------------- | -------- | ------------------------------------ |
| `version`   | `string`         | Yes      | Tool version                         |
| `as`        | `string`         | No       | Rename binary in PATH                |
| `source`    | `#Source`        | No       | Default source (Homebrew if omitted) |
| `overrides` | `[...#Override]` | No       | Platform-specific sources            |

### #Override

Platform-specific source override.

```cue
overrides: [
    {os: "linux", arch: "arm64", source: schema.#GitHub & {...}}
]
```

**Fields:**

| Field    | Type      | Required | Description                   |
| -------- | --------- | -------- | ----------------------------- |
| `os`     | `#OS`     | No       | Match by OS (darwin, linux)   |
| `arch`   | `#Arch`   | No       | Match by arch (arm64, x86_64) |
| `source` | `#Source` | Yes      | Source for matching platforms |

### Tool Sources

#### #Homebrew

Fetches from Homebrew bottles (ghcr.io/homebrew). This is the default source.

```cue
source: schema.#Homebrew & {
    formula: "go@1.24"  // Optional: override formula name
}
```

**Fields:**

| Field     | Type     | Required | Description                          |
| --------- | -------- | -------- | ------------------------------------ |
| `formula` | `string` | No       | Formula name (defaults to tool name) |

#### #GitHub

Downloads from GitHub Releases. Supports template variables: `{version}`, `{os}`, `{arch}`.

```cue
source: schema.#GitHub & {
    repo: "oven-sh/bun"
    tag: "bun-v{version}"
    asset: "bun-darwin-aarch64.zip"
    path: "bun-darwin-aarch64/bun"
}
```

**Fields:**

| Field   | Type     | Required | Description                                 |
| ------- | -------- | -------- | ------------------------------------------- |
| `repo`  | `string` | Yes      | Repository (owner/repo)                     |
| `tag`   | `string` | No       | Release tag (default: "v{version}")         |
| `asset` | `string` | Yes      | Asset name (supports template variables)    |
| `path`  | `string` | No       | Path to binary within archive (if archived) |

#### #Oci

Extracts binaries from OCI container images.

```cue
source: schema.#Oci & {
    image: "ghcr.io/org/tool:{version}"
    path: "/usr/local/bin/tool"
}
```

**Fields:**

| Field   | Type     | Required | Description                          |
| ------- | -------- | -------- | ------------------------------------ |
| `image` | `string` | Yes      | Image reference (supports templates) |
| `path`  | `string` | Yes      | Path to binary inside the container  |

#### #Nix

Builds from a Nix flake.

```cue
source: schema.#Nix & {
    flake: "nixpkgs"  // Key from runtime.flakes
    package: "jq"
}
```

**Fields:**

| Field     | Type     | Required | Description                           |
| --------- | -------- | -------- | ------------------------------------- |
| `flake`   | `string` | Yes      | Named flake reference (key in flakes) |
| `package` | `string` | Yes      | Package attribute                     |
| `output`  | `string` | No       | Output path if auto-detection fails   |

#### #Rustup

Manages Rust toolchains via rustup. Supports version pinning, installation profiles, components, and cross-compilation targets.

```cue
source: schema.#Rustup & {
    toolchain: "1.83.0"
    profile: "default"
    components: ["clippy", "rustfmt", "rust-src"]
    targets: ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]
}
```

**Fields:**

| Field        | Type          | Required | Default     | Description                                                |
| ------------ | ------------- | -------- | ----------- | ---------------------------------------------------------- |
| `toolchain`  | `string`      | Yes      | -           | Toolchain identifier (e.g., "stable", "1.83.0", "nightly") |
| `profile`    | `string`      | No       | `"default"` | Installation profile                                       |
| `components` | `[...string]` | No       | `[]`        | Additional components to install                           |
| `targets`    | `[...string]` | No       | `[]`        | Cross-compilation targets                                  |

**Profiles:**

| Profile    | Included Components       |
| ---------- | ------------------------- |
| `minimal`  | rustc, rust-std, cargo    |
| `default`  | minimal + rustfmt, clippy |
| `complete` | All available components  |

**Common Components:**

| Component            | Description                         |
| -------------------- | ----------------------------------- |
| `clippy`             | Lint tool                           |
| `rustfmt`            | Code formatter                      |
| `rust-src`           | Source code (for IDE support)       |
| `llvm-tools-preview` | LLVM tools (for code coverage)      |
| `rust-analyzer`      | LSP server (bundled with toolchain) |

**Common Targets:**

| Target                      | Description    |
| --------------------------- | -------------- |
| `x86_64-unknown-linux-gnu`  | Linux x86_64   |
| `aarch64-unknown-linux-gnu` | Linux ARM64    |
| `x86_64-apple-darwin`       | macOS x86_64   |
| `aarch64-apple-darwin`      | macOS ARM64    |
| `wasm32-unknown-unknown`    | WebAssembly    |
| `x86_64-pc-windows-msvc`    | Windows x86_64 |

:::note[Prerequisite]
Rustup must be installed on the system. Install via: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
:::

### #ToolsActivate

Hook for shell integration that activates tools on directory entry.

```cue
hooks: {
    onEnter: {
        tools: schema.#ToolsActivate
    }
}
```

**Definition:**

```cue
#ToolsActivate: #ExecHook & {
    order:     10
    propagate: false
    command:   "cuenv"
    args:      ["tools", "activate"]
    source:    true
    inputs:    ["cuenv.lock"]
}
```

:::note
For `cuenv exec` and `cuenv task`, tools are activated automatically without requiring this hook. Use `#ToolsActivate` for interactive shell integration.
:::

## CI Configuration

cuenv can generate CI workflow manifests for multiple providers. CI configuration requires **explicit opt-in** - no workflows are generated unless providers are specified.

### #CIProvider

Supported CI provider names for workflow generation.

```cue
#CIProvider: "github" | "buildkite" | "gitlab"
```

### #CI

Root CI configuration for the project.

```cue
import "github.com/cuenv/cuenv/schema"

ci: {
    // CI providers to emit workflows for (explicit opt-in required)
    // If not specified, no workflows are generated
    providers: ["github"]

    // Provider-specific configuration
    provider: github: {
        runner: "ubuntu-latest"
        flakehubCache: true
    }

    // Pipeline definitions
    pipelines: {
        ci: {
            when: { pullRequest: true }
            tasks: ["check"]
        }
    }

    // Contributors that inject tasks into the DAG
    contributors: [...]
}
```

**Fields:**

| Field          | Type                        | Required | Description                                      |
| -------------- | --------------------------- | -------- | ------------------------------------------------ |
| `providers`    | `[...#CIProvider]`          | No       | CI providers to emit workflows for               |
| `pipelines`    | `{[string]: #Pipeline}`     | No       | Named pipeline definitions                       |
| `provider`     | `#ProviderConfig`           | No       | Provider-specific configuration                  |
| `contributors` | `[...#Contributor]`         | No       | Contributors that inject tasks into the DAG      |

:::note[Explicit Opt-In]
If `providers` is not specified, **no CI workflows are emitted**. You must explicitly configure which providers to generate manifests for.
:::

### #Pipeline

Individual pipeline configuration.

```cue
pipelines: {
    release: {
        // Override global providers for this pipeline
        providers: ["buildkite"]

        environment: "production"
        when: { release: ["published"] }
        tasks: ["build", "publish"]
    }
}
```

**Fields:**

| Field         | Type                 | Required | Description                                       |
| ------------- | -------------------- | -------- | ------------------------------------------------- |
| `providers`   | `[...#CIProvider]`   | No       | Override global providers (completely replaces)   |
| `mode`        | `#PipelineMode`      | No       | Generation mode: "thin" (default) or "expanded"   |
| `environment` | `string`             | No       | Environment for secret resolution                 |
| `when`        | `#PipelineCondition` | No       | Trigger conditions                                |
| `tasks`       | `[...#PipelineTask]` | No       | Tasks to run                                      |
| `provider`    | `#ProviderConfig`    | No       | Provider-specific overrides                       |

**Provider Override Behavior:** Per-pipeline `providers` **completely replaces** the global `ci.providers` - there is no merging.

### #PipelineCondition

Trigger conditions for pipeline execution.

```cue
when: {
    pullRequest: true                    // Run on PRs
    branch: "main"                       // Run on specific branch
    tag: "v*"                            // Run on tags matching pattern
    defaultBranch: true                  // Run on default branch
    scheduled: "0 0 * * *"               // Cron schedule
    manual: true                         // Allow manual dispatch
    release: ["published"]               // Run on release events
}
```

### Example: GitHub Packages Publishing

Generated GitHub Actions workflows seed standard GitHub context into cuenv execution steps. Publishing
tasks should still explicitly opt in to the values they consume with `#EnvPassthrough`:

```cue
import "github.com/cuenv/cuenv/schema"

let #GitHubPublishEnv = {
    GITHUB_TOKEN:    schema.#EnvPassthrough
    GITHUB_ACTOR:   schema.#EnvPassthrough
    GITHUB_REF_TYPE: schema.#EnvPassthrough
    GITHUB_REF_NAME: schema.#EnvPassthrough
}

ci: {
    providers: ["github"]
    provider: github: permissions: packages: "write"
}

tasks: {
    publish: schema.#Task & {
        env: #GitHubPublishEnv
        command: "bash"
        args: ["-c", """
            set -euo pipefail

            : "${GITHUB_TOKEN:?GITHUB_TOKEN is required}"
            : "${GITHUB_ACTOR:?GITHUB_ACTOR is required}"

            echo "actor_present=yes ref_type=${GITHUB_REF_TYPE:-unset} ref_name=${GITHUB_REF_NAME:-unset}"
            oras version

            echo "$GITHUB_TOKEN" | oras login ghcr.io -u "$GITHUB_ACTOR" --password-stdin

            # existing publish commands here
        """]
    }
}
```

No custom contributor is needed for this pattern. For local test runs, export the same variables
yourself or the shell guards will fail before registry login.

### Example: Multi-Provider Configuration

```cue
ci: {
    // Global default: emit GitHub Actions workflows
    providers: ["github"]

    provider: github: {
        runner: "ubuntu-latest"
    }

    pipelines: {
        // Uses global providers (GitHub Actions)
        ci: {
            when: { pullRequest: true, branch: "main" }
            tasks: ["check"]
        }

        // Override: emit Buildkite pipeline for release
        release: {
            providers: ["buildkite"]
            environment: "production"
            when: { release: ["published"] }
            tasks: ["build", "publish"]
        }
    }
}
```

## See Also

- [Configuration Guide](/how-to/configure-a-project/) - Usage patterns
- [Tools Guide](/how-to/tools/) - Tools configuration and usage
- [Tools Architecture](/explanation/tools/) - How the tools system works internally
- [CI Contributors Reference](/reference/ci-contributors/) - Contributor system documentation
- [API Reference](/reference/rust-api/) - Rust API documentation
- [Examples](/reference/examples/) - Complete examples
