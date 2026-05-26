---
title: Architecture
description: cuenv system architecture overview
---

This page describes the internal architecture of cuenv, explaining how the different components work together to provide typed environment management and task orchestration.

## System Overview

cuenv is built as a layered system with clear separation of concerns:

```
┌─────────────────────────────────────────────────────────────┐
│                      cuenv-cli                              │
│           (Command-line interface & user interaction)       │
├─────────────────────────────────────────────────────────────┤
│                      cuenv-core                             │
│        (Tasks, environments, hooks, secrets, shell)         │
├─────────────────────────────────────────────────────────────┤
│                       cuengine                              │
│              (CUE evaluation via Go FFI)                    │
├─────────────────────────────────────────────────────────────┤
│                    Go CUE Runtime                           │
│              (cuelang.org/go/cue)                           │
└─────────────────────────────────────────────────────────────┘
```

## Crate Structure

### cuengine

The CUE evaluation engine providing a safe Rust interface to the Go-based CUE evaluator.

**Key responsibilities:**

- FFI bridge between Rust and Go
- Safe memory management with RAII wrappers
- CUE expression evaluation
- Caching of evaluation results
- Retry logic for transient failures

**Notable features:**

- `evaluate_module()` - Module-wide CUE evaluation for efficient cross-project operations
- `evaluate_cue_package()` - Single-directory evaluation
- `CStringPtr` - RAII wrapper for C strings returned from FFI
- Response envelope parsing with structured error handling

```rust
use cuengine::evaluate_module;
use std::path::Path;

let raw = evaluate_module(Path::new("./project"), "cuenv", None)?;
// Access raw.instances and raw.projects
```

### cuenv-core

Core library containing shared types, configuration parsing, and domain logic.

**Modules:**

- `manifest` - CUE manifest parsing and the `Cuenv` type
- `tasks` - Task definitions, execution, and dependencies
- `environment` - Environment variable handling and validation
- `hooks` - Shell hooks for onEnter/onExit events
- `secrets` - Secret resolution and policy enforcement
- `shell` - Shell integration and export formatting
- `cache` - Task caching with content-aware invalidation
- `config` - Configuration file handling

Manifest schema types stay re-exported from `crates/core/src/manifest/mod.rs`,
while the implementation is split by schema concern: hooks, VCS dependencies,
formatters, codegen, directory rules, runtimes, services/images, and project
conversion each live in sibling modules under `crates/core/src/manifest/`.
Task schema support types follow the same pattern: `crates/core/src/tasks/mod.rs`
keeps the executable task and task-tree model, while params, retry config,
legacy task-level Dagger config, cache policy, capture metadata, shell
configuration, input references, and dependency references live in sibling
modules under `crates/core/src/tasks/`. The task executor keeps
graph orchestration in `crates/core/src/tasks/executor.rs`; command redaction
helpers, failure-summary formatting, and workspace-root detection live in
`command.rs`, `result.rs`, and `workspace.rs` beside it.
Core task graph wrapping stays in `crates/core/src/tasks/graph.rs`; task path
resolution for dotted/bracketed CUE task nodes lives in
`crates/core/src/tasks/graph/resolver.rs`. Generic task DAG primitives live in
`crates/task-graph/src/graph.rs`, while resolver-backed group expansion and
sequence ordering live in `crates/task-graph/src/graph/resolver_build.rs`.
Task contributor injection is owned by `crates/core/src/contributors.rs`; the
built-in package-manager workspace contributor definitions live in
`crates/core/src/contributors/workspace.rs` so DAG mutation stays separate from
the built-in Bun/npm/pnpm/Yarn setup catalog.
Persistent hook execution state, marker files, cleanup, and execution hashes
live under `crates/hooks/src/state/`; hook execution orchestration lives in
`crates/hooks/src/executor.rs`; source-hook shell environment capture lives in
`crates/hooks/src/executor/source_environment.rs`.
Environment schema values, policy checks, interpolation, and secret-aware value
resolution live in `crates/core/src/environment/values.rs`; runtime
environment merging, PATH lookup, and filtered task/exec/service environment
construction stay in `crates/core/src/environment.rs`.

**Error handling:**
Uses `miette` for rich diagnostic errors with:

- Source code snippets
- Error spans
- Contextual help messages
- Suggestions for fixes

```rust
use cuenv_core::{Error, Result};

#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(cuenv::config::invalid),
        help("Check your env.cue configuration")
    )]
    Configuration { message: String },
}
```

### cuenv (CLI)

The command-line interface built with `clap`.

**Commands:**

- `cuenv task [name]` - Execute or list tasks
- `cuenv env print|check|load|list` - Environment operations
- `cuenv exec -- <cmd>` - Run commands with environment
- `cuenv shell init <shell>` - Generate shell integration
- `cuenv allow|deny` - Security approval management
- `cuenv sync` - Generate files from CUE configuration (e.g., ignore files)
- `cuenv changeset|release` - Release management
- `cuenv version` - Version information

CLI parser setup lives in `crates/cuenv/src/cli.rs`; the top-level Clap
command enum lives in `crates/cuenv/src/cli/commands.rs`; nested subcommand
enums live in `crates/cuenv/src/cli/subcommands.rs`; conversion into the
internal command model lives in `crates/cuenv/src/cli/command_conversion.rs`;
output formats and JSON envelopes live in `crates/cuenv/src/cli/output.rs`;
and CLI error mapping and rendering live in `crates/cuenv/src/cli/error.rs`.
Executor dispatch lives in `crates/cuenv/src/commands/dispatch.rs`. Startup,
runtime selection, and early process routing stay in `crates/cuenv/src/main.rs`;
command paths that bypass the executor are isolated in
`crates/cuenv/src/sync_dispatch.rs` and `crates/cuenv/src/async_dispatch.rs`,
while the hook supervisor process runs through
`crates/cuenv/src/hook_supervisor.rs` and OCI activation runs through
`crates/cuenv/src/oci_activate.rs`. Release command flows stay under
`crates/cuenv/src/commands/release.rs`; binary artifact orchestration lives in
`crates/cuenv/src/commands/release/binaries.rs`, and release-prepare analysis,
version application, git publication, and PR creation live in
`crates/cuenv/src/commands/release/prepare.rs`. Task-list data construction
stays in `crates/cuenv/src/commands/task_list.rs`; text, rich, tables,
dashboard, and emoji renderers live in
`crates/cuenv/src/commands/task_list/formatters.rs`.
The rich TUI keeps event-driven task/output state in
`crates/cuenv/src/tui/state/activity.rs`, input-driven view state in
`crates/cuenv/src/tui/state/view.rs`, and uses
`crates/cuenv/src/tui/state.rs` as the coordinator that preserves the
renderer-facing accessors and event-application boundary.
Sync command provider adapters live under
`crates/cuenv/src/commands/sync/providers/`; shared sync command
orchestration remains in `crates/cuenv/src/commands/sync/functions.rs`, and
codegen file check/write/diff behavior lives in
`crates/cuenv/src/commands/sync/functions/codegen.rs`. GitHub workflow
sync and non-matrix workflow-shape emission helpers live in
`crates/cuenv/src/commands/sync/functions/github.rs`; matrix workflow
expansion and artifact aggregation live in
`crates/cuenv/src/commands/sync/functions/github/matrix.rs`.
The lock provider keeps path/workspace orchestration in
`crates/cuenv/src/commands/sync/providers/lock.rs`, while multi-source tool
resolution, cache reuse, source template expansion, and provider registry
setup live in `crates/cuenv/src/commands/sync/providers/lock/tool_resolution.rs`.
The VCS sync provider keeps orchestration in
`crates/cuenv/src/commands/sync/providers/vcs.rs`, checkout/marker verification
in `crates/cuenv/src/commands/sync/providers/vcs/materialization.rs`, and
path/temp safety in `crates/cuenv/src/commands/sync/providers/vcs/paths.rs`.
GitHub Actions release workflow construction is separated into
`crates/github/src/workflow/release.rs`; bootstrap, simple, matrix, and
artifact job construction lives in `crates/github/src/workflow/jobs.rs`;
the general emitter stays focused on workflow naming, triggers, permissions,
and serialization.

### cuenv-ci

CI support compiles project tasks and CUE-defined CI contributors into an
intermediate representation before provider emitters render workflow files.
The compiler entrypoint lives in `crates/ci/src/compiler/mod.rs`; contributor
activation, provider-condition checks, priority-to-stage mapping, and
contributor task conversion live in `crates/ci/src/compiler/contributors.rs`.
Pipeline trigger condition assembly, normalized repo-relative path filters, and
workspace dependency trigger paths live in
`crates/ci/src/compiler/triggers.rs`.
CI execution and garbage collection are decomposed into explicit planning,
execution, reporting, cache-scan, sweep, and finalization helpers instead of
depending on broad complexity suppressions. CI task DAG execution and IR runner
setup live in `crates/ci/src/executor/task_execution.rs`. CI task tool download
and lockfile activation support lives in `crates/ci/src/executor/tools.rs`; CI
hook-backed environment assembly lives in `crates/ci/src/executor/hook_env.rs`,
keeping the orchestrator focused on pipeline scheduling. CI report writing,
provider notification, annotation resolution, and CI redaction setup live in
`crates/ci/src/executor/reporting.rs`; per-task environment precedence and
passthrough handling live in `crates/ci/src/executor/task_env.rs`.

### cuenv-1password

1Password secret resolution auto-selects between HTTP mode via the WASM SDK and
CLI mode via the `op` command. Resolver mode selection, WASM client lifecycle,
and HTTP batch resolution live in `crates/1password/src/secrets/resolver.rs`;
CLI authentication preflight, signed-out bootstrap reads, and `op read`
execution live in `crates/1password/src/secrets/cli.rs`.

### cuenv-events

The canonical event path constructs typed `CuenvEvent` values and publishes
them through the process-wide sender. The compatibility tracing layer in
`crates/events/src/layer.rs` still translates tracing fields into typed events;
field extraction and category-specific event construction stay separated so
output, task, service, CI, command, interactive, and system events do not share
one monolithic conversion path.

### cuenv-workspaces

Workspace management for monorepos.

**Features:**

- Detect and configure workspaces
- Package manager integration (bun, pnpm, yarn, cargo)
- Workspace-aware task execution

Cargo lockfile parsing keeps workspace member discovery in
`crates/workspaces/src/parsers/rust/cargo/workspace.rs`, separate from
`Cargo.lock` package entry and `SourceId` conversion in
`crates/workspaces/src/parsers/rust/cargo.rs`.

## FFI Bridge

The Go-Rust FFI bridge is a critical component enabling CUE evaluation from Rust.

### Go Side (bridge.go)

```go
// Exported functions callable from Rust
//export cue_eval_package
func cue_eval_package(pathPtr *C.char, packagePtr *C.char) *C.char

//export cue_free_string
func cue_free_string(ptr *C.char)

//export cue_bridge_version
func cue_bridge_version() *C.char
```

### Rust Side (cuengine)

```rust
// FFI declarations
extern "C" {
    fn cue_eval_package(path: *const c_char, package: *const c_char) -> *mut c_char;
    fn cue_free_string(ptr: *mut c_char);
    fn cue_bridge_version() -> *mut c_char;
}
```

### Response Envelope

All FFI responses use a structured JSON envelope:

```json
{
  "version": "1.0.0",
  "ok": {
    /* evaluation result */
  },
  "error": null
}
```

Or on error:

```json
{
  "version": "1.0.0",
  "ok": null,
  "error": {
    "code": "LOAD_INSTANCE",
    "message": "failed to load CUE instance",
    "hint": "Check that the package exists"
  }
}
```

### Error Codes

| Code                 | Description                    |
| -------------------- | ------------------------------ |
| `INVALID_INPUT`      | Invalid input parameters       |
| `LOAD_INSTANCE`      | Failed to load CUE instance    |
| `BUILD_VALUE`        | Failed to build CUE value      |
| `ORDERED_JSON`       | JSON serialization failed      |
| `PANIC_RECOVER`      | Recovered from Go panic        |
| `JSON_MARSHAL_ERROR` | JSON marshaling failed         |
| `REGISTRY_INIT`      | Registry initialization failed |

## Caching Architecture

cuenv implements multiple caching layers:

### CUE Evaluation Cache

LRU cache for CUE evaluation results:

```rust
pub struct EvaluationCache {
    cache: LruCache<CacheKey, CachedResult>,
    max_size: usize,
}
```

**Cache key components:**

- Directory path
- Package name
- File content hashes

### Task Cache

Content-aware caching for task outputs:

```
~/.cache/cuenv/how-to/run-tasks/<hash>/
├── metadata.json    # Task metadata and input hashes
├── stdout           # Captured stdout
├── stderr           # Captured stderr
└── outputs/         # Task output files
```

**Cache invalidation triggers:**

- Input file changes (content hash)
- Task definition changes
- Environment variable changes
- cuenv version changes

## Task Execution Model

### Dependency Resolution

Tasks form a directed acyclic graph (DAG) based on `dependsOn`:

```
        ┌─────┐
        │build│
        └──┬──┘
           │
     ┌─────┴─────┐
     ▼           ▼
  ┌─────┐    ┌─────┐
  │lint │    │test │
  └─────┘    └──┬──┘
               │
            ┌──┴──┐
            ▼     ▼
         ┌────┐┌────┐
         │unit││e2e │
         └────┘└────┘
```

### Execution Strategies

1. **Sequential** (array tasks): Execute in order
2. **Parallel** (object tasks): Execute concurrently
3. **Dependent**: Wait for dependencies to complete

### Task Types

```cue
tasks: {
    // Single command
    build: {
        command: "cargo"
        args: ["build"]
    }

    // Sequential list
    deploy: [
        {command: "build"},
        {command: "push"},
    ]

    // Nested (parallel groups)
    test: {
        unit: {command: "test-unit"}
        e2e: {command: "test-e2e"}
    }
}
```

## Shell Integration

### Hook Lifecycle

```
cd /project/dir
       │
       ▼
┌──────────────┐
│ cuenv detect │
└──────┬───────┘
       │ env.cue found
       ▼
┌──────────────┐
│ Check approval│
└──────┬───────┘
       │ approved
       ▼
┌──────────────┐
│ Load hooks   │
│ (background) │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Execute      │
│ onEnter      │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Export env   │
│ to shell     │
└──────────────┘
```

### Security Model

1. **Approval Gate**: Configurations must be explicitly approved before hooks run
2. **Content Hashing**: Approval is invalidated when configuration changes
3. **Policy Enforcement**: Secrets are only accessible to authorized tasks

The shell export formatter keeps the loaded-directory and pending-approval
environment variable names as explicit constants that are interpolated into
each supported shell snippet.

## Data Flow

### Environment Loading

```
env.cue ──► cuengine ──► Cuenv struct ──► Environment vars
   │                         │
   └── schema validation ────┘
```

### Task Execution

```
cuenv task build
       │
       ▼
┌──────────────┐
│ Parse config │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Resolve deps │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Check cache  │
└──────┬───────┘
       │ miss
       ▼
┌──────────────┐
│ Load env     │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Resolve      │
│ secrets      │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Execute      │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ Update cache │
└──────────────┘
```

## Future Architecture (Planned)

### Security Isolation

- **Linux namespaces**: Process isolation
- **Landlock**: Filesystem access control
- **eBPF**: System call monitoring

### Distributed Execution

- Remote cache backends
- Distributed task execution
- Build farm integration

## See Also

- [Configuration Schema](/reference/cue-schema/) - Schema definitions
- [Contributing](/how-to/contribute/) - Development setup
- [API Reference](/reference/rust-api/) - Detailed API documentation
