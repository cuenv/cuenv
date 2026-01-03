---
title: API Reference
description: Complete API reference for cuenv
---

This page documents the public APIs of cuenv's Rust crates. For schema definitions, see [Configuration Schema](/reference/cue-schema/).

## cuengine

The CUE evaluation engine crate provides the interface to evaluate CUE configurations through the Go FFI bridge.

### `evaluate_module` (Recommended)

The recommended entry point for CUE evaluation. Evaluates an entire CUE module at once, returning all instances (projects and bases) in a single call. This is more efficient than per-directory evaluation when working with monorepos.

```rust
use cuengine::{evaluate_module, ModuleEvaluation};
use cuenv_core::module::find_cue_module_root;
use cuenv_core::manifest::Project;
use std::path::Path;

// Find the module root (directory containing cue.mod/)
let project_path = Path::new("./my-project");
let module_root = find_cue_module_root(project_path)?;

// Evaluate the entire module with a specific package
let raw_json = evaluate_module(&module_root, "cuenv", None)?;

// Parse into ModuleEvaluation for easy access
let module = ModuleEvaluation::from_raw(&module_root, &raw_json)?;

// Access specific project by relative path
let instance = module.get(Path::new("my-project"))?;
let project: Project = instance.deserialize()?;

// Iterate all projects in the module
for instance in module.projects() {
    println!("Project at: {}", instance.path.display());
}
```

**Key types:**

| Type               | Description                                                    |
| ------------------ | -------------------------------------------------------------- |
| `ModuleEvaluation` | Wrapper around evaluated module with helper methods            |
| `Instance`         | Single evaluated instance (project or base) with path and kind |
| `InstanceKind`     | Enum: `Project` (has name field) or `Base` (no name field)     |

**ModuleEvaluation methods:**

| Method         | Description                                         |
| -------------- | --------------------------------------------------- |
| `from_raw()`   | Parse raw JSON into structured module evaluation    |
| `get(path)`    | Get instance at relative path                       |
| `projects()`   | Iterator over all Project instances                 |
| `bases()`      | Iterator over all Base instances                    |
| `ancestors(p)` | Get ancestor instances for a path (for inheritance) |

**Instance methods:**

| Method          | Description                                 |
| --------------- | ------------------------------------------- |
| `deserialize()` | Deserialize instance data into typed struct |
| `kind`          | Whether this is a Project or Base           |
| `path`          | Relative path within the module             |

### `evaluate_cue_package`

Free function for single-directory evaluation. Use `evaluate_module()` for module-wide operations:

```rust
use cuengine::{evaluate_cue_package, evaluate_cue_package_typed};
use cuenv_core::manifest::Cuenv;
use std::path::Path;

let json = evaluate_cue_package(Path::new("./project"), "cuenv")?;
let manifest: Cuenv = evaluate_cue_package_typed(Path::new("./project"), "cuenv")?;
```

### `get_bridge_version`

Fetches the Go bridge version string for diagnostics:

```rust
let version = cuengine::get_bridge_version()?;
println!("bridge reports {version}");
```

### RetryConfig

Configuration for retry behavior on transient failures.

```rust
use cuengine::RetryConfig;
use std::time::Duration;

let config = RetryConfig {
    max_attempts: 4,
    initial_delay: Duration::from_millis(100),
    max_delay: Duration::from_secs(5),
    exponential_base: 2.0,
};
```

**Fields:**

| Field              | Type       | Default | Description                                 |
| ------------------ | ---------- | ------- | ------------------------------------------- |
| `max_attempts`     | `u32`      | `3`     | Maximum retry attempts                      |
| `initial_delay`    | `Duration` | 100 ms  | Delay before the first retry                |
| `max_delay`        | `Duration` | 10 s    | Upper bound for the backoff delay           |
| `exponential_base` | `f32`      | `2.0`   | Multiplier applied to each successive delay |

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

#### Task

Represents a single executable command pulled from CUE.

```rust
use cuenv_core::tasks::Task;
use serde_json::json;
use std::collections::HashMap;

let mut env = HashMap::new();
env.insert("RUST_LOG".into(), json!("info"));

let build = Task {
    command: "cargo".into(),
    args: vec!["build".into(), "--release".into()],
    shell: None,
    env,
    depends_on: vec!["lint".into(), "test".into()],
    inputs: vec!["src".into(), "Cargo.toml".into()],
    outputs: vec!["target/release/app".into()],
    external_inputs: None,
    workspaces: vec![],
    description: Some("Build release binaries".into()),
};
```

**Fields:**

| Field             | Type                                 | Description                                         |
| ----------------- | ------------------------------------ | --------------------------------------------------- |
| `command`         | `String`                             | Executable to run                                   |
| `args`            | `Vec<String>`                        | Arguments for the command                           |
| `shell`           | `Option<Shell>`                      | Override shell invocation (defaults to direct exec) |
| `env`             | `HashMap<String, serde_json::Value>` | Task-specific environment additions                 |
| `depends_on`      | `Vec<String>`                        | Other tasks that must finish first                  |
| `inputs`          | `Vec<Input>`                         | Files/globs or task output references               |
| `outputs`         | `Vec<String>`                        | Declared outputs that become cacheable artifacts    |
| `external_inputs` | `Option<Vec<ExternalInput>>`         | Consume outputs from another project in the repo    |
| `workspaces`      | `Vec<String>`                        | Workspace names to enable (see schema)              |
| `description`     | `Option<String>`                     | Human-friendly summary                              |

#### TaskDefinition & TaskGroup

```rust
use cuenv_core::tasks::{TaskDefinition, TaskGroup};

let test_task = build.clone(); // assume another Task definition exists

let group = TaskDefinition::Group(TaskGroup::Sequential(vec![
    TaskDefinition::Single(Box::new(build.clone())),
    TaskDefinition::Single(Box::new(test_task)),
]));
```

- `TaskDefinition::Single(Task)` represents one command.
- `TaskDefinition::Group(TaskGroup)` represents sequential (`Vec<TaskDefinition>`) or parallel (`HashMap<String, TaskDefinition>`) sub-tasks.

#### Tasks

`Tasks` is the top-level map of task names to their definitions (flattened when parsed from CUE).

```rust
use cuenv_core::tasks::{TaskDefinition, Tasks};

let mut tasks = Tasks::new();
tasks.tasks.insert("build".into(), TaskDefinition::Single(Box::new(build)));
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

## cuenv-hooks

Hook execution, state management, and approval system. This crate was extracted from cuenv-core to provide a focused API for hook management.

### Hooks

Shell hook definitions.

```rust
use cuenv_hooks::{Hook, Hooks};
```

**Fields:**

| Field      | Type                    | Description                     |
| ---------- | ----------------------- | ------------------------------- |
| `on_enter` | `Option<Vec<Hook>>`     | Hooks to run on directory entry |
| `on_exit`  | `Option<Vec<Hook>>`     | Hooks to run on directory exit  |

### Hook

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

### HookExecutor

Manages background hook execution.

```rust
use cuenv_hooks::{HookExecutor, HookExecutionConfig};

let config = HookExecutionConfig::default();
let executor = HookExecutor::new(config)?;
```

### ApprovalManager

Manages hook configuration approvals for security.

```rust
use cuenv_hooks::{ApprovalManager, check_approval_status, ApprovalStatus};
use std::path::Path;

let manager = ApprovalManager::with_default_file()?;
let status = check_approval_status(&manager, Path::new("."), hooks.as_ref())?;

match status {
    ApprovalStatus::Approved => println!("Config is approved"),
    ApprovalStatus::RequiresApproval { current_hash } => {
        println!("Needs approval, hash: {}", current_hash);
    }
    ApprovalStatus::NotApproved { current_hash } => {
        println!("Not approved, hash: {}", current_hash);
    }
}
```

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
    Shell::PowerShell => { /* PowerShell */ }
}

assert!(Shell::Bash.is_supported());
assert!(!Shell::PowerShell.is_supported());
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
use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::Project;
use std::path::Path;

fn run() -> Result<()> {
    let manifest: Project = evaluate_cue_package_typed(Path::new("."), "cuenv")?;
    println!("Loaded project: {:?}", manifest.name);
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

#### Task cache helpers

The task cache utilities live under `cuenv_core::cache::tasks`.

```rust
use cuenv_core::cache::tasks::{
    compute_cache_key, lookup, materialize_outputs, CacheKeyEnvelope,
};
use std::{collections::BTreeMap, path::Path};

let envelope = CacheKeyEnvelope {
    inputs: BTreeMap::new(),
    command: "cargo".into(),
    args: vec!["build".into()],
    shell: None,
    env: BTreeMap::new(),
    cuenv_version: cuenv_core::VERSION.to_string(),
    platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    workspace_lockfile_hashes: None,
    workspace_package_hashes: None,
};

let (key, _) = compute_cache_key(&envelope)?;
if let Some(entry) = lookup(&key, None) {
    println!("cache hit stored at {}", entry.path.display());
    materialize_outputs(&key, Path::new("artifacts"), None)?;
}
```

Additional helpers such as `save_result`, `record_latest`, and `lookup_latest` are available when integrating custom executors with cuenv's cache layout.

## CLI Exit Codes

The cuenv CLI uses structured exit codes:

| Code | Name        | Description                                                                     |
| ---- | ----------- | ------------------------------------------------------------------------------- |
| 0    | Success     | Command completed successfully                                                  |
| 2    | ConfigError | CLI/configuration error (`CliError::Config`)                                    |
| 3    | EvalError   | Evaluation, task, or other runtime error (`CliError::Eval` / `CliError::Other`) |

## See Also

- [Configuration Schema](/reference/cue-schema/) - CUE schema definitions
- [Architecture](/explanation/architecture/) - System design overview
- [Contributing](/how-to/contribute/) - Development guide
