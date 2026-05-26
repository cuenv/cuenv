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

CLI parsing lives in `crates/cuenv/src/cli.rs`, conversion into the internal
command model lives in `crates/cuenv/src/cli/command_conversion.rs`, and
executor dispatch lives in `crates/cuenv/src/commands/dispatch.rs`. Startup,
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
Sync command provider adapters live under
`crates/cuenv/src/commands/sync/providers/`; shared sync command
orchestration remains in `crates/cuenv/src/commands/sync/functions.rs`, and
codegen file check/write/diff behavior lives in
`crates/cuenv/src/commands/sync/functions/codegen.rs`. GitHub workflow
sync and workflow-shape emission helpers live in
`crates/cuenv/src/commands/sync/functions/github.rs`.

### cuenv-ci

CI support compiles project tasks and CUE-defined CI contributors into an
intermediate representation before provider emitters render workflow files.
The compiler entrypoint lives in `crates/ci/src/compiler/mod.rs`; contributor
activation, provider-condition checks, priority-to-stage mapping, and
contributor task conversion live in `crates/ci/src/compiler/contributors.rs`.
CI execution and garbage collection are decomposed into explicit planning,
execution, reporting, cache-scan, sweep, and finalization helpers instead of
depending on broad complexity suppressions. CI task tool download and lockfile
activation support lives in `crates/ci/src/executor/tools.rs`; CI hook-backed
environment assembly lives in `crates/ci/src/executor/hook_env.rs`, keeping
the orchestrator focused on pipeline scheduling. CI report writing, provider
notification, annotation resolution, and CI redaction setup live in
`crates/ci/src/executor/reporting.rs`.

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
