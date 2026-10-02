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
use cuengine::evaluate_module;
use cuenv_core::ModuleEvaluation;
use cuenv_core::cue::discovery::find_cue_module_root;
use cuenv_core::manifest::Project;
use std::path::Path;

// Find the module root (directory containing cue.mod/)
let project_path = Path::new("./my-project");
let module_root = find_cue_module_root(project_path).ok_or("no CUE module")?;

// Evaluate the entire module with a specific package
let result = evaluate_module(&module_root, "cuenv", None)?;

// Wrap the directory-keyed instances for easy access
let module = ModuleEvaluation::from_raw(module_root, result.instances, result.projects, None);

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

### `ModuleEvalOptions`

`evaluate_module(module_root, package, Some(&options))` takes
`cuengine::ModuleEvalOptions`. Every field has a default
(`..Default::default()`); the defaults keep the behaviour described above.

| Field                          | Default  | Effect                                                                                  |
| ------------------------------ | -------- | --------------------------------------------------------------------------------------- |
| `recursive`                    | `false`  | `true` evaluates the module tree (`./...`), `false` one directory                       |
| `package_name`                 | `None`   | The package to evaluate (takes precedence over the legacy `package` argument)           |
| `target_dir`                   | `None`   | The directory of a non-recursive evaluation (default: the module root)                  |
| `package_scope`                | `Named`  | `All` evaluates every package of every directory; see keys below                        |
| `instance_failures`            | `Skip`   | `Fail` fails the call when any loaded instance fails to load, build, validate or export |
| `concrete_paths`               | empty    | Paths that must exist and be fully concrete in every instance                           |
| `export_paths`                 | empty    | When set, export only these paths of each instance                                      |
| `presence_paths`               | empty    | Report which of these paths exist in each instance, without exporting them              |
| `skipped_directories`          | `Ignore` | `Report` lists the directories a recursive evaluation did not load                      |
| `task_field`                   | `None`   | The top-level field that holds the task graph; see below                                |
| `with_meta`, `with_references` | `false`  | Source positions and reference paths in `ModuleResult::meta`                            |

**Task field.** Sequence items inside the task graph need a hidden `_name`
field before export so that output references resolve. `task_field` names the
top-level field that holds the graph: sequence items below it get `_name`
injected, and a projection (`export_paths`) that exports nothing below that
field skips the injection. `None` keeps the behaviour every existing caller
relies on (the field is `tasks`); `Some("")` turns the injection off, so a
caller that evaluates something other than a cuenv project does not depend on
cuenv's task shape. The injection itself (and the project detection) is still
cuenv-specific code in the bridge.

**Instance keys.** With `PackageScope::Named`, `ModuleResult::instances` is
keyed by the directory relative to the module root (`"."`,
`"services/api"`); these keys are paths, and `ModuleEvaluation::from_raw`
treats them as such. With `PackageScope::All` a key is
`"<directory>:<package>"` (`".:app"`, `"services/api:worker"`, and
`"<directory>:_"` for files without a package clause); split it at the last
`:` (package names are identifiers) and do not pass these keys to
`ModuleEvaluation::from_raw`, which would read them as directory names.
`projects` uses the same keys as `instances`; `meta` keys are
`"<instance key>/<field path>"`. No package name may be combined with
`PackageScope::All`.

**Failures.** With `InstanceFailures::Fail` the error lists every failed
instance as `<key>: <errors>`, each CUE error with its field path and file
positions relative to the module root. A failure the loader reports before
it knows the package (for example two packages in one directory under
`PackageScope::Named`) is named by its directory alone, or by the load
pattern (`./...`) when it has no directory, so in an all-packages listing a
bare directory is not a package key. Evaluation errors are
`CueEngineError::CueParse` ("CUE evaluation failed at <root>: …"); invalid
options are `CueEngineError::Configuration`.

**Projection.** `export_paths` and `presence_paths` take CUE paths of regular
fields (`name`, `config.database`, `"quoted-label".items`); list indices,
definitions, hidden fields and an empty path are a configuration error. An
exported object keeps the nesting (`config.database` exports
`{"config": {"database": …}}`) and leaves out paths an instance lacks. A path
is present when the instance has a regular field there; a field only
declared optional (`field?:`) is not present. Projection changes the export
only: every instance is still loaded, built and checked, so
`instance_failures` and `concrete_paths` behave as without it. A module-wide
check that needs a few fields of every instance should use it:

```rust
use cuengine::{
    evaluate_module, InstanceFailures, ModuleEvalOptions, PackageScope, SkippedDirectories,
};

let options = ModuleEvalOptions {
    recursive: true,
    package_scope: PackageScope::All,
    instance_failures: InstanceFailures::Fail,
    export_paths: vec!["name".to_string()],
    presence_paths: vec!["infrastructure".to_string()],
    skipped_directories: SkippedDirectories::Report,
    ..Default::default()
};
let result = evaluate_module(&module_root, "", Some(&options))?;
for (key, value) in &result.instances {
    let name = value.get("name").and_then(|name| name.as_str());
    let has_infrastructure = result
        .present
        .get(key)
        .is_some_and(|paths| paths.iter().any(|path| path == "infrastructure"));
    // ...
}
for skipped in &result.skipped_directories {
    // skipped.path (relative, `/`-separated) and skipped.reason
}
```

**Skipped directories.** A recursive evaluation follows CUE's `./...` rules:
below the evaluated directory, directories whose name starts with `.` or
`_`, directories named `testdata`, and directories holding their own
`cue.mod` are not loaded, nor are unreadable directories. The evaluated
directory itself is always loaded, whatever its name (a module rooted at
`.config` or `_work` evaluates normally). With
`SkippedDirectories::Report`, `ModuleResult::skipped_directories` lists each
left-out directory that holds a `.cue` file at any depth (only the topmost
one of a tree), with its reason (`Dot`, `Underscore`, `Testdata`,
`NestedModule` or `Unreadable`), so a caller that must see every instance can
refuse to continue. The list is also returned when nothing matched.

**Memory.** Each instance is built in its own CUE context and exported
before the next one is built, so a large module is not held in memory all
at once.

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

cuenv uses a **Task API v2** with explicit task types for clear semantics.

#### TaskNode

The main task type enum with three variants:

```rust
use cuenv_core::tasks::{TaskNode, Task, TaskGroup, TaskDependency};

// TaskNode::Task - single executable command
let build = TaskNode::Task(Task {
    command: Some("cargo".into()),
    args: vec!["build".into(), "--release".into()],
    description: Some("Build release binaries".into()),
    depends_on: vec![
        TaskDependency::from_name("lint"),
        TaskDependency::from_name("test"),
    ],
    inputs: vec!["src/**/*.rs".into(), "Cargo.toml".into()],
    outputs: vec!["target/release/app".into()],
    ..Default::default()
});

// TaskNode::Group - parallel execution
let checks = TaskNode::Group(TaskGroup {
    type_: "group".into(),
    children: [
        ("lint".into(), TaskNode::Task(Task { command: Some("cargo".into()), args: vec!["clippy".into()], ..Default::default() })),
        ("test".into(), TaskNode::Task(Task { command: Some("cargo".into()), args: vec!["test".into()], ..Default::default() })),
    ].into_iter().collect(),
    ..Default::default()
});

// TaskNode::Sequence - sequential execution
let deploy = TaskNode::Sequence(vec![
    TaskNode::Task(Task { command: Some("build".into()), ..Default::default() }),
    TaskNode::Task(Task { command: Some("push".into()), ..Default::default() }),
]);
```

**TaskNode variants:**

| Variant                  | Description                                      |
| ------------------------ | ------------------------------------------------ |
| `TaskNode::Task(Task)`   | Single executable command or script              |
| `TaskNode::Group(TaskGroup)` | Parallel execution - all children run concurrently |
| `TaskNode::Sequence(Vec<TaskNode>)` | Sequential execution - runs in order |

#### Task

Represents a single executable command.

**Fields:**

| Field             | Type                                 | Description                                         |
| ----------------- | ------------------------------------ | --------------------------------------------------- |
| `command`         | `Option<String>`                     | Command to execute                                  |
| `args`            | `Vec<String>`                        | Command arguments                                   |
| `script`          | `Option<String>`                     | Multi-line script (alternative to command)          |
| `script_shell`    | `Option<ScriptShell>`                | Shell for script execution (default: bash)          |
| `shell_options`   | `Option<ShellOptions>`               | POSIX shell options for `bash`/`zsh`, or `sh` with `pipefail: false` |
| `env`             | `HashMap<String, serde_json::Value>` | Task-specific environment additions                 |
| `depends_on`      | `Vec<TaskDependency>`                | Task dependencies (resolved from CUE references)    |
| `inputs`          | `Vec<Input>`                         | Files/globs or task output references               |
| `outputs`         | `Vec<String>`                        | Declared outputs that become cacheable artifacts    |
| `description`     | `Option<String>`                     | Human-friendly summary                              |
| `hermetic`        | `Option<bool>`                       | Isolated execution (default: true)                  |
| `timeout`         | `Option<String>`                     | Execution timeout (e.g., "30m")                     |
| `continue_on_error` | `Option<bool>`                     | Continue on failure (default: false)                |

#### TaskDependency

Task dependencies are resolved from CUE references, providing compile-time validation:

```rust
use cuenv_core::tasks::TaskDependency;

// Create a dependency by name
let dep = TaskDependency::from_name("build");

// Get the task name
let name: &str = dep.task_name();
```

#### TaskGroup

Parallel execution group - all child tasks run concurrently.

**Fields:**

| Field            | Type                           | Description                              |
| ---------------- | ------------------------------ | ---------------------------------------- |
| `type_`          | `String`                       | Type discriminator (always "group")      |
| `children`       | `HashMap<String, TaskNode>`    | Named child tasks (run in parallel)      |
| `depends_on`     | `Vec<TaskDependency>`          | Dependencies on other tasks              |
| `max_concurrency`| `Option<i32>`                  | Limit concurrent executions (0 = unlimited) |
| `description`    | `Option<String>`               | Human-readable description               |

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

The task cache utilities live under the `cuenv-cache` crate.

```rust
use cuenv_cache::{
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

## cuenv-infrastructure

- **Provider environment.** `cuenv_secrets::RESOLVER_ENVIRONMENT_VARIABLES`
  is the table of variables only cuenv's secret machinery uses, each an exact
  name or a prefix (`NamePattern`) and a `ValueKind` (a `Credential` is
  withheld and redacted, an `Endpoint` such as `OP_CONNECT_HOST` is only
  withheld). `is_resolver_credential`, `is_resolver_environment_variable`,
  `resolver_environment_variable_names` and `resolver_credential_values` query
  it. The command withholds the matching variables from providers in `inherit`
  mode unless the project passes the same name, and `main` registers the
  credential values for redaction from the same table.
  `cuenv_infrastructure::plugin::ISOLATED_INHERITED_ENVIRONMENT_VARIABLES`
  is the allowlist of the `isolated` mode and `isolated_withheld_names` computes
  the names to pass in `withheld_environment_variables` for it. cuenv's own
  handshake variables are set after the withheld names are removed, so a policy
  can never strip them. Variables whose names are not valid unicode are never
  passed to a provider. This is hygiene, not a sandbox (see the how-to guide).
- `Cancellation::stop()` is the first interrupt: no new resource is started and
  every provider the engine launched is asked to stop.
  `terminate_providers()` is the second: it kills them (with their process
  groups) and removes their socket directories; then
  `wait_for_recordings(bound)` lets a record being written finish before the
  lock is released. The command calls them from its SIGINT, SIGTERM, SIGHUP
  and SIGQUIT handling, which it installs before evaluating anything.

### Redaction (`cuenv-events`)

`register_secret` and `register_secrets` take the values to hide. A multi-line
secret is also registered line by line, and a secret with characters that are
escaped when quoted is also registered in its debug-quoted, JSON-quoted and Go
JSON (`\u0026` for `&`) forms. `redact` (and `redact_cow`, which does not copy
text without a secret) replaces them in text in one pass over a matcher that is
compiled when the registry changes; where secrets overlap the whole stretch is
replaced. `redact_json_value` and `redact_json_text` replace them inside the
string values of a JSON value and keep the keys, so a secret that JSON escapes
is still found and no secret renames a field; `redact_json_value_and_keys` and
`redact_free_form_json_text` also redact keys, for free-form JSON such as a
provider's log. `CuenvEvent::redacted` rewrites each event by type, so no
secret can rename a tag or withhold an event. `emit_with_source` redacts every
event before any subscriber sees it, the CLI and JSON renderers redact again,
and `RedactingStderr` (with `RedactingWriter` and `LogFormat`) redacts tracing's
formatting layers. Values shorter than `MIN_SECRET_LENGTH` (4) are ignored.

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
