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
cuenv-specific code in the bridge; see the design specification's next steps.

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

| Variant                             | Description                                        |
| ----------------------------------- | -------------------------------------------------- |
| `TaskNode::Task(Task)`              | Single executable command or script                |
| `TaskNode::Group(TaskGroup)`        | Parallel execution - all children run concurrently |
| `TaskNode::Sequence(Vec<TaskNode>)` | Sequential execution - runs in order               |

#### Task

Represents a single executable command.

**Fields:**

| Field               | Type                                 | Description                                                          |
| ------------------- | ------------------------------------ | -------------------------------------------------------------------- |
| `command`           | `Option<String>`                     | Command to execute                                                   |
| `args`              | `Vec<String>`                        | Command arguments                                                    |
| `script`            | `Option<String>`                     | Multi-line script (alternative to command)                           |
| `script_shell`      | `Option<ScriptShell>`                | Shell for script execution (default: bash)                           |
| `shell_options`     | `Option<ShellOptions>`               | POSIX shell options for `bash`/`zsh`, or `sh` with `pipefail: false` |
| `env`               | `HashMap<String, serde_json::Value>` | Task-specific environment additions                                  |
| `depends_on`        | `Vec<TaskDependency>`                | Task dependencies (resolved from CUE references)                     |
| `inputs`            | `Vec<Input>`                         | Files/globs or task output references                                |
| `outputs`           | `Vec<String>`                        | Declared outputs that become cacheable artifacts                     |
| `description`       | `Option<String>`                     | Human-friendly summary                                               |
| `hermetic`          | `Option<bool>`                       | Isolated execution (default: true)                                   |
| `timeout`           | `Option<String>`                     | Execution timeout (e.g., "30m")                                      |
| `continue_on_error` | `Option<bool>`                       | Continue on failure (default: false)                                 |

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

| Field             | Type                        | Description                                 |
| ----------------- | --------------------------- | ------------------------------------------- |
| `type_`           | `String`                    | Type discriminator (always "group")         |
| `children`        | `HashMap<String, TaskNode>` | Named child tasks (run in parallel)         |
| `depends_on`      | `Vec<TaskDependency>`       | Dependencies on other tasks                 |
| `max_concurrency` | `Option<i32>`               | Limit concurrent executions (0 = unlimited) |
| `description`     | `Option<String>`            | Human-readable description                  |

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

| Field      | Type                | Description                     |
| ---------- | ------------------- | ------------------------------- |
| `on_enter` | `Option<Vec<Hook>>` | Hooks to run on directory entry |
| `on_exit`  | `Option<Vec<Hook>>` | Hooks to run on directory exit  |

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

`cuenv infrastructure` is a thin command over the `cuenv-infrastructure` crate,
which other tools can drive the same way. The lock is released and the
providers are stopped whatever the outcome, so no `?` may leave them behind:

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use cuenv_infrastructure::{
    ApplyContext, Cancellation, EngineOptions, EngineSetup, InfrastructureEngine,
    InfrastructureError, LockRequest, OwnerClaim, OwnerClaimMode, PlanMode, ProjectInstance,
    StateLock,
};

let cancellation = Cancellation::default(); // shared with signal handling
let instance = ProjectInstance::new("services/api", "cuenv")?; // directory and package
store.migrate().await?; // writes need current tables; reads never create them
// Choose the identifier first and register it with signal handling, so a
// forced exit during acquisition can name the lock it may hold.
let lock = StateLock::generate();
store
    .acquire_lock(&tenant, &LockRequest { lock: &lock, holder: "my tool" })
    .await?;
let mut engine = InfrastructureEngine::new(EngineSetup {
    tenant: tenant.clone(), // TenantKey: module path, project name, optional environment
    store: Arc::clone(&store), // Arc<dyn StateStore>, such as TursoStateStore
    infrastructure,            // the selected configuration, see `Infrastructure::select`
    options: EngineOptions {
        project_directory,
        plugin_cache_directory: None,
        // Names providers must not inherit, applied after the overlay below.
        withheld_environment_variables: vec!["TURSO_AUTH_TOKEN".into()],
        // Resolved, policy-authorized project values added to each provider's environment.
        provider_environment_variables: BTreeMap::new(),
        unrecorded_directory: None, // the user state directory
        cancellation: cancellation.clone(),
    },
});
let outcome = async {
    // The first lock records the owning instance; another instance is refused.
    store
        .claim_owner(&tenant, &lock, &OwnerClaim { instance: &instance, mode: OwnerClaimMode::IfUnowned })
        .await?
        .require(&tenant, &instance)?;
    let plan = engine.plan(PlanMode::Apply).await?;
    // Show the plan and ask for confirmation here, still holding the lock.
    if plan.has_work() {
        engine.apply(&plan, ApplyContext { lock: &lock }, &mut |_event| {}).await?;
    }
    Ok::<_, InfrastructureError>(plan)
}
.await;
let released = store.unlock(&tenant, &lock).await;
engine.shutdown().await;
let plan = outcome?;
released?;
```

- **Identities.** `TenantKey::new(module_path, project)` is the identity of a
  run without `--env`; `TenantKey::with_environment(module_path, project,
environment)` is a separate identity for a named environment (including
  `default`), and its `Display` form is `<module>#<project>@<environment>`.
  Every store method takes a `TenantKey`, and nothing falls back from one
  identity to another. The Turso store keeps one table family keyed by
  `(module_path, project, environment, …)`, with an empty environment for the
  no-flag identity, at schema version 1; `migrate()` fails with
  `InfrastructureError::StateSchemaNewer` for a newer schema and, for any later
  migration, with `InfrastructureError::StateMigrationBlocked` while a lock row
  exists. A row that cannot be decoded is `InfrastructureError::UndecodableRecord`
  naming the address.
- **Configuration.** `cuenv_manifest::manifest::Infrastructure::select(value,
environment)` strictly decodes the raw `infrastructure` value of a project
  for the selected environment (`None` for the top level) and returns an
  `InfrastructureSelectionError` that names the declared environments when
  the requested one is missing. `cuenv_infrastructure::validate_configuration(&infrastructure,
environment)` repeats the schema's semantic checks and reports **every**
  problem, each with its full field path
  (`infrastructure.environments.dev.resources.pet.dependsOn[0]`).
  `InfrastructureConfiguration` is the exported type of one configuration, and
  `ProviderEnvironment` (`Inherit`, `Isolated`) is the manifest form of
  `providerEnvironment`.
- **Provider environment.** `cuenv_secrets::RESOLVER_CREDENTIAL_ENVIRONMENT_VARIABLES`
  lists the variables only cuenv's secret resolvers use; the command withholds
  them from providers in `inherit` mode unless the project passes the same
  name. `cuenv_infrastructure::plugin::ISOLATED_INHERITED_ENVIRONMENT_VARIABLES`
  is the allowlist of the `isolated` mode and `isolated_withheld_names` computes
  the names to pass in `withheld_environment_variables` for it. cuenv's own
  handshake variables are set after the withheld names are removed, so a policy
  can never strip them.
- `Cancellation::stop()` is the first interrupt: no new resource is started and
  every provider the engine launched is asked to stop.
  `terminate_providers()` is the second: it kills them (with their process
  groups) and removes their socket directories; then
  `wait_for_recordings(bound)` lets a record being written finish before the
  lock is released. The command calls them from its SIGINT, SIGTERM, SIGHUP
  and SIGQUIT handling, which it installs before evaluating anything.
- `Plan::has_work()` is true when applying would change infrastructure or
  rewrite stored records (`PlanSummary::refresh`). `engine.plan()` orders the
  changes as one dependency graph and refuses cycles and changes no order can
  apply, so the plan lists its changes in apply order and refusals happen
  before any confirmation. `engine.apply()` refuses a plan whose stored records
  changed since it was made (`InfrastructureError::PlanOutdated`) and a plan
  made with other provider environment values
  (`InfrastructureError::PlanEnvironmentChanged`); `Plan::digest()` identifies
  everything a plan would do. The command plans and confirms while holding
  the lock, as Terraform does, so it applies exactly the plan shown.
- **Failed applies.** A provider failure skips the operations that depend on
  it and lets the rest run; `apply` then returns
  `InfrastructureError::ApplyIncomplete(IncompleteApply)` with the failures,
  the skipped changes and the replacements deleted but not recreated.
  `ApplyEvent::DeletedNotRecreated` reports each such replacement on every way
  an apply can end early, and `ApplyEvent::Failed` and `ApplyEvent::Skipped`
  report the others.
- `StateStore::owner()` names the CUE instance (`ProjectInstance`,
  `<directory>:<package>`) that owns a tenant's state; `claim_owner()` with
  `OwnerClaimMode::IfUnowned` records it under the first lock, and
  `OwnerClaimMode::Transfer` is an explicit adoption
  (`cuenv infrastructure state adopt`). `TenantOwner::require()` refuses any
  other instance (`InfrastructureError::OwnedByAnotherInstance`).
- A change the store could not record is saved by `UnrecordedStore` in the
  user state directory in one file format (`formatVersion` 1: tenant with its
  environment, generation, serial and backend binding); `engine.plan()` refuses
  to run until `UnrecordedStore::recover` has recorded it under the lock
  (`cuenv infrastructure state recover`). `has_pending()` checks without a
  lock. `RecoverOverrides` holds two independent, per-file overrides:
  `changed_record: ChangedRecord::Overwrite` writes over a stored record that
  changed since the change was saved (otherwise
  `InfrastructureError::StateChanged`, which names the file), and
  `backend: BackendMismatch::Accept` accepts a file saved for another state
  backend or without a binding. They are the CLI's `--force` and
  `--accept-backend`; neither implies the other, and the default
  (`RecoverOverrides::default()`) overrides nothing. `localhost`, `127.0.0.1`
  and `[::1]` are one backend.

### Redaction (`cuenv-events`)

`register_secret` and `register_secrets` take the values to hide. A multi-line
secret is also registered line by line, and a secret with characters that are
escaped when quoted is also registered in its debug-quoted and JSON-quoted
forms. `redact` replaces them in text, and `redact_json_value` and
`redact_json_text` replace them inside the strings of a JSON value, so a secret
that JSON escapes is still found. `emit_with_source` redacts every event before
any subscriber sees it, the CLI and JSON renderers redact again, and
`RedactingStderr` (with `RedactingWriter` and `LogFormat`) redacts tracing's
formatting layers. Values shorter than `MIN_SECRET_LENGTH` (4) are ignored.
Redaction clones and re-serializes each event while any secret is registered;
see the design specification's next steps.

## CLI Exit Codes

The cuenv CLI uses structured exit codes:

| Code | Constant              | Description                                                                                                                                            | JSON `code`                  |
| ---- | --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------- |
| 0    | `EXIT_OK`             | Command completed successfully                                                                                                                         | (success envelope)           |
| 1    | `EXIT_CANCELLED`      | Infrastructure change not confirmed at the prompt; nothing was applied (`CliError::Infrastructure` with `Cancelled` kind)                              | `infrastructure_cancelled`   |
| 2    | `EXIT_CLI`            | CLI/configuration error (`CliError::Config`)                                                                                                           | `config`                     |
| 3    | `EXIT_EVAL`           | Evaluation, task, or other runtime error (`CliError::Eval` / `CliError::Other`)                                                                        | `eval` / `other`             |
| 4    | `EXIT_LOCKED`         | Infrastructure run collided with concurrent activity; retrying later can succeed (`CliError::Infrastructure` with `Locked` kind)                       | `infrastructure_locked`      |
| 5    | `EXIT_INFRASTRUCTURE` | Any other infrastructure failure: provider, state store, apply, ownership or unrecorded changes (`CliError::Infrastructure` with `Failed` kind)        | `infrastructure`             |
| 130  | `EXIT_INTERRUPTED`    | Interrupted: Ctrl-C for every command; for `cuenv infrastructure` also SIGTERM, SIGHUP or SIGQUIT (`CliError::Infrastructure` with `Interrupted` kind) | `infrastructure_interrupted` |

`exit_code_for` and `error_code_for` in `cuenv::cli` map a `CliError` to its
exit code and to the `code` field of the JSON error envelope, which
`error_envelope` builds:
`{"status":"error","error":{"code":…,"message":…,"help":…,"lockIdentifier":…,"lockReleased":…,"deletedNotRecreated":…}}`.
`help` is present when the error has help text; `lockIdentifier` and
`lockReleased` are present when an infrastructure error concerns a state lock
(`CliError::with_lock` and `LockStatus`), and `lockReleased` is `false`
whenever the release failed or is unknown; `deletedNotRecreated` lists the
replacements a failed or interrupted apply deleted without recreating
(`CliError::with_deleted_not_recreated`). Every `CliError` constructor redacts
its message and help from the raw strings, `error_report_text` redacts the
terminal report before it is wrapped, and `error_envelope` redacts each string
of the envelope. Codes 1, 4, 5 and 130 with an
envelope are only produced by `cuenv infrastructure`; they extend the taxonomy
of ADR-0005. `InfrastructureFailureKind` (`Locked`, `Cancelled`,
`Interrupted`, `Failed`) selects between them. `cuenv infrastructure` itself
exits with 3 only for CUE evaluation errors (`eval`). `EXIT_INTERRUPTED` is
the single constant for 130: other commands interrupted by Ctrl-C exit with
it too, without an envelope.

## See Also

- [Configuration Schema](/reference/cue-schema/) - CUE schema definitions
- [Architecture](/explanation/architecture/) - System design overview
- [Contributing](/how-to/contribute/) - Development guide
