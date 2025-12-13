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

- `CueEvaluator` - Main evaluator type with builder pattern configuration
- `CStringPtr` - RAII wrapper for C strings returned from FFI
- Response envelope parsing with structured error handling
- LRU cache for evaluation results

```rust
use cuengine::CueEvaluatorBuilder;

let evaluator = CueEvaluatorBuilder::new()
    .directory("./project")
    .package("cuenv")
    .build()?;

let result = evaluator.evaluate()?;
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
- `cuenv tui` - Interactive event dashboard
- `cuenv changeset|release` - Release management
- `cuenv version` - Version information

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
