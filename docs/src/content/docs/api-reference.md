---
title: API Reference
description: Complete API reference for cuenv
---

This page documents the public APIs of cuenv's Rust crates. For schema definitions, see [Configuration Schema](/configuration-schema/).

## cuengine

The CUE evaluation engine crate provides the interface to evaluate CUE configurations.

### CueEvaluatorBuilder

Builder for creating CUE evaluators with configurable options.

```rust
use cuengine::CueEvaluatorBuilder;

let evaluator = CueEvaluatorBuilder::new()
    .directory("./project")
    .package("cuenv")
    .build()?;
```

**Methods:**

| Method            | Description                                         |
| ----------------- | --------------------------------------------------- |
| `new()`           | Create a new builder                                |
| `directory(path)` | Set the directory containing CUE files              |
| `package(name)`   | Set the CUE package name to evaluate                |
| `build()`         | Build the evaluator, returns `Result<CueEvaluator>` |

### CueEvaluator

The main evaluator type for CUE expressions.

```rust
let result = evaluator.evaluate()?;
```

**Methods:**

| Method           | Return Type      | Description                             |
| ---------------- | ---------------- | --------------------------------------- |
| `evaluate()`     | `Result<Cuenv>`  | Evaluate CUE and return parsed manifest |
| `evaluate_raw()` | `Result<String>` | Evaluate CUE and return raw JSON        |

### RetryConfig

Configuration for retry behavior on transient failures.

```rust
use cuengine::RetryConfig;

let config = RetryConfig {
    max_retries: 3,
    initial_delay_ms: 100,
    max_delay_ms: 5000,
    backoff_factor: 2.0,
};
```

**Fields:**

| Field              | Type  | Default | Description                    |
| ------------------ | ----- | ------- | ------------------------------ |
| `max_retries`      | `u32` | 3       | Maximum retry attempts         |
| `initial_delay_ms` | `u64` | 100     | Initial delay between retries  |
| `max_delay_ms`     | `u64` | 5000    | Maximum delay cap              |
| `backoff_factor`   | `f64` | 2.0     | Exponential backoff multiplier |

## cuenv-core

Core library with types for tasks, environments, hooks, and secrets.

### Cuenv (Manifest)

The root configuration type parsed from CUE.

```rust
use cuenv_core::manifest::Cuenv;

let manifest: Cuenv = evaluator.evaluate()?;
```

**Fields:**

| Field        | Type                     | Description             |
| ------------ | ------------------------ | ----------------------- |
| `config`     | `Option<Config>`         | Global configuration    |
| `env`        | `Option<Env>`            | Environment variables   |
| `hooks`      | `Option<Hooks>`          | Shell hooks             |
| `tasks`      | `HashMap<String, Tasks>` | Task definitions        |
| `workspaces` | `Option<Workspaces>`     | Workspace configuration |

### Task Types

#### SingleTask

A single command execution.

```rust
use cuenv_core::tasks::SingleTask;

let task = SingleTask {
    command: "cargo".to_string(),
    args: vec!["build".to_string()],
    env: None,
    shell: None,
    depends_on: None,
    inputs: None,
    outputs: None,
};
```

**Fields:**

| Field        | Type                             | Description               |
| ------------ | -------------------------------- | ------------------------- |
| `command`    | `String`                         | Command to execute        |
| `args`       | `Vec<String>`                    | Command arguments         |
| `env`        | `Option<HashMap<String, Value>>` | Task-specific environment |
| `shell`      | `Option<Shell>`                  | Shell to use              |
| `depends_on` | `Option<Vec<String>>`            | Task dependencies         |
| `inputs`     | `Option<Vec<String>>`            | Input file patterns       |
| `outputs`    | `Option<Vec<String>>`            | Output file patterns      |

#### Tasks (enum)

Represents different task structures.

```rust
use cuenv_core::tasks::Tasks;

match task {
    Tasks::Single(single) => { /* SingleTask */ }
    Tasks::List(list) => { /* Vec<SingleTask> - sequential */ }
    Tasks::Group(group) => { /* HashMap<String, Tasks> - nested */ }
}
```

### Environment

#### Env

Environment variable definitions.

```rust
use cuenv_core::environment::Env;
```

Environment values can be:

- Simple values (strings, numbers, booleans)
- Structured values with policies
- Secret references

### Hooks

#### Hooks

Shell hook definitions.

```rust
use cuenv_core::hooks::{Hooks, ExecHook};
```

**Fields:**

| Field      | Type                    | Description                     |
| ---------- | ----------------------- | ------------------------------- |
| `on_enter` | `Option<Vec<ExecHook>>` | Hooks to run on directory entry |
| `on_exit`  | `Option<Vec<ExecHook>>` | Hooks to run on directory exit  |

#### ExecHook

A single hook execution.

**Fields:**

| Field       | Type          | Default  | Description                       |
| ----------- | ------------- | -------- | --------------------------------- |
| `command`   | `String`      | required | Command to execute                |
| `args`      | `Vec<String>` | `[]`     | Command arguments                 |
| `order`     | `i32`         | 0        | Execution order (lower = earlier) |
| `propagate` | `bool`        | false    | Export to child processes         |
| `source`    | `bool`        | false    | Source output as shell script     |
| `inputs`    | `Vec<String>` | `[]`     | Input files for cache tracking    |

### Secrets

#### Secret

Secret reference with exec-based resolution.

```rust
use cuenv_core::secrets::Secret;
```

**Fields:**

| Field      | Type          | Description                   |
| ---------- | ------------- | ----------------------------- |
| `resolver` | `String`      | Resolver type (always "exec") |
| `command`  | `String`      | Command to retrieve secret    |
| `args`     | `Vec<String>` | Command arguments             |

#### Policy

Access control policy for secrets.

```rust
use cuenv_core::secrets::Policy;
```

**Fields:**

| Field         | Type          | Description                   |
| ------------- | ------------- | ----------------------------- |
| `allow_tasks` | `Vec<String>` | Tasks that can access         |
| `allow_exec`  | `Vec<String>` | Exec commands that can access |

### Shell

Shell integration types.

#### Shell (enum)

Supported shell types.

```rust
use cuenv_core::shell::Shell;

match shell {
    Shell::Bash => { /* Bash shell */ }
    Shell::Zsh => { /* Zsh shell */ }
    Shell::Fish => { /* Fish shell */ }
    Shell::Nushell => { /* Nushell */ }
}
```

### Error Handling

#### Error

The main error type with diagnostic information.

```rust
use cuenv_core::Error;
```

**Variants:**

| Variant         | Description                            |
| --------------- | -------------------------------------- |
| `Configuration` | Configuration parsing/validation error |
| `Ffi`           | FFI operation failure                  |
| `CueParse`      | CUE parsing error                      |
| `Io`            | I/O operation failure                  |
| `Task`          | Task execution error                   |
| `Secret`        | Secret resolution error                |
| `Shell`         | Shell integration error                |

All errors implement `miette::Diagnostic` for rich error reporting:

```rust
use miette::Result;

fn run() -> Result<()> {
    let evaluator = CueEvaluatorBuilder::new()
        .directory(".")
        .package("cuenv")
        .build()?;

    let manifest = evaluator.evaluate()?;
    Ok(())
}
```

### Type Wrappers

#### PackageDir

Validated directory path.

```rust
use cuenv_core::PackageDir;
use std::path::Path;

let pkg_dir = PackageDir::try_from(Path::new("./project"))?;
```

#### PackageName

Validated CUE package name.

```rust
use cuenv_core::PackageName;

let pkg_name = PackageName::try_from("cuenv")?;
```

### Cache

#### TaskCache

Task output caching.

```rust
use cuenv_core::cache::TaskCache;

let cache = TaskCache::new()?;
let key = cache.compute_key(&task, &inputs, &env)?;

if let Some(hit) = cache.get(&key)? {
    // Use cached result
} else {
    // Execute task
    cache.put(&key, &result)?;
}
```

## CLI Exit Codes

The cuenv CLI uses structured exit codes:

| Code | Name         | Description                             |
| ---- | ------------ | --------------------------------------- |
| 0    | Success      | Operation completed successfully        |
| 1    | GeneralError | Generic error                           |
| 2    | ConfigError  | Configuration parsing/validation failed |
| 3    | TaskError    | Task execution failed                   |
| 4    | NotApproved  | Configuration not approved              |
| 5    | NotFound     | Requested resource not found            |
| 64   | UsageError   | Invalid command line usage              |

## See Also

- [Configuration Schema](/configuration-schema/) - CUE schema definitions
- [Architecture](/architecture/) - System design overview
- [Contributing](/contributing/) - Development guide
