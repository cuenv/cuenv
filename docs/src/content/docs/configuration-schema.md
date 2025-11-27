---
title: Configuration Schema
description: Schema definitions and validation for cuenv configurations
---

This page documents the CUE schema definitions used in cuenv configurations. Import these from `github.com/cuenv/cuenv/schema` in your `env.cue` files.

## Root Schema

### #Cuenv

The root configuration type that validates your entire `env.cue` file.

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

// Your configuration here
env: {...}
tasks: {...}
```

**Fields:**

| Field        | Type                 | Required | Description                      |
| ------------ | -------------------- | -------- | -------------------------------- |
| `config`     | `#Config`            | No       | Global configuration options     |
| `env`        | `#Env`               | No       | Environment variable definitions |
| `hooks`      | `#Hooks`             | No       | Shell hooks for onEnter/onExit   |
| `tasks`      | `{[string]: #Tasks}` | Yes      | Task definitions                 |
| `workspaces` | `#Workspaces`        | No       | Workspace configuration          |

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

### #Tasks

A task can be a single command, a list (sequential), or a group (parallel/nested).

```cue
tasks: {
    // Single task
    build: {
        command: "cargo"
        args: ["build"]
    }

    // Sequential list
    deploy: [
        {command: "build"},
        {command: "push"},
    ]

    // Nested group
    test: {
        unit: {command: "cargo", args: ["test", "--lib"]}
        e2e: {command: "cargo", args: ["test", "--test", "e2e"]}
    }
}
```

### #Task

A single task definition.

```cue
tasks: {
    build: {
        // Required
        command: "cargo"

        // Optional
        args: ["build", "--release"]
        shell: schema.#Bash
        env: {
            RUST_LOG: "debug"
        }
        dependsOn: ["lint", "test"]
        inputs: ["src/**/*.rs", "Cargo.toml"]
        outputs: ["target/release/myapp"]
        description: "Build the application"
        workspaces: ["packages/core"]
    }
}
```

**Fields:**

| Field            | Type                               | Required | Description                      |
| ---------------- | ---------------------------------- | -------- | -------------------------------- |
| `command`        | `string`                           | Yes      | Command to execute               |
| `args`           | `[...string]`                      | No       | Command arguments                |
| `shell`          | `#Shell`                           | No       | Shell to use for execution       |
| `env`            | `{[string]: #EnvironmentVariable}` | No       | Task-specific environment        |
| `dependsOn`      | `[...string]`                      | No       | Task dependencies                |
| `inputs`         | `[...string]`                      | No       | Input file patterns for caching  |
| `outputs`        | `[...string]`                      | No       | Output file patterns for caching |
| `description`    | `string`                           | No       | Human-readable description       |
| `workspaces`     | `[...string]`                      | No       | Workspaces to enable             |
| `externalInputs` | `[...#ExternalInput]`              | No       | Cross-project dependencies       |

### #TaskGroup

Task groups determine execution mode by structure:

```cue
// Array = Sequential execution
sequential: [
    {command: "step1"},
    {command: "step2"},
    {command: "step3"},
]

// Object = Parallel/nested execution
parallel: {
    task1: {command: "cmd1"}
    task2: {command: "cmd2"}
    nested: {
        subtask: {command: "cmd3"}
    }
}
```

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
    propagate: true
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

## See Also

- [Configuration Guide](/configuration/) - Usage patterns
- [API Reference](/api-reference/) - Rust API documentation
- [Examples](/examples/) - Complete examples
