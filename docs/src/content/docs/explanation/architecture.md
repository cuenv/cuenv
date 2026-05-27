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
The crate root does not carry broad derive-workaround lint allowances; warnings
should be fixed or scoped to the module that actually owns the exceptional code.

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
Module-wide CUE evaluation assembly lives in `crates/core/src/module.rs`;
task-reference/source enrichment lives in `crates/core/src/module/task_refs.rs`
and `crates/core/src/module/task_sources.rs`; instance deserialize diagnostics
live in `crates/core/src/module/deserialize.rs`.
Task schema support types follow the same pattern: `crates/core/src/tasks/mod.rs`
keeps the executable task and task-tree model, while params, retry config,
legacy task-level Dagger config, cache policy, capture metadata, shell
configuration, input references, and dependency references live in sibling
modules under `crates/core/src/tasks/`. The task executor keeps
graph orchestration in `crates/core/src/tasks/executor.rs`; host process
spawning, process-registry lifecycle, output streaming, and result assembly
live in `crates/core/src/tasks/process.rs`. Unix process-group setup is kept in
that host-process boundary so `pre_exec` and signal-tree management do not leak
into task orchestration. Command redaction helpers,
failure-summary formatting, and workspace-root detection live in `command.rs`,
`result.rs`, and `workspace.rs` beside it.
Core task graph wrapping stays in `crates/core/src/tasks/graph.rs`; task graph
construction lives in `crates/core/src/tasks/graph/build.rs`, task output-ref
dependency edge materialization lives in
`crates/core/src/tasks/graph/output_refs.rs`, and task path resolution for
dotted/bracketed CUE task nodes lives in `crates/core/src/tasks/graph/resolver.rs`.
Generic task DAG primitives live in `crates/task-graph/src/graph.rs`;
affected-task and transitive-closure helpers live in
`crates/task-graph/src/graph/analysis.rs`, while resolver-backed group expansion
and sequence ordering live in
`crates/task-graph/src/graph/resolver_build.rs`. Read-only DAG callers only
need `TaskNodeData`; resolver-backed expansion and output-reference injection
require `MutableTaskNodeData` so dependency mutation is explicit instead of a
default panic on the base trait. Service/image orchestration uses read-only
mixed graph nodes with declared dependency names only.
Task contributor schema models, activation context, DAG injection, and DAG
verification helpers live under `crates/core/src/contributors/`; the built-in
package-manager workspace contributor definitions live in
`crates/core/src/contributors/workspace.rs` so DAG mutation stays separate from
the built-in Bun/npm/pnpm/Yarn setup catalog.
Persistent hook execution state, marker files, cleanup, and execution hashes
live under `crates/hooks/src/state/`, including integer duration display
formatting; hook execution orchestration lives in `crates/hooks/src/executor.rs`,
including saturating elapsed-millisecond conversion for persisted hook results;
source-hook shell environment capture lives in
`crates/hooks/src/executor/source_environment.rs`. CLI hook command
orchestration lives in `crates/cuenv/src/commands/hooks.rs`; shell-init
snippet generation and status rendering live under
`crates/cuenv/src/commands/hooks/`.
Environment schema values, policy checks, interpolation, and secret-aware value
resolution live in `crates/core/src/environment/values.rs`; runtime
environment merging, PATH lookup, and filtered task/exec/service environment
construction stay in `crates/core/src/environment.rs`. CLI task execution
keeps feature-gated executor construction in
`crates/cuenv/src/commands/task/mod.rs`, while
`crates/cuenv/src/commands/task/execution.rs` owns task selection, runtime
preparation, and execution orchestration.

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
Static completion setup text is routed through the shared redacted stdout helper
instead of raw print macros, even though it contains no secrets.
Dynamic completions currently complete task names via discovery-based CUE
evaluation; unused task-parameter completion scaffolding is not kept in-tree.
The sync fast-path version command and `--llms` output use the same helper so
static stdout output does not need local print suppressions.
The event-driven version command emits progress from an explicit static step
table, keeping user-facing progress messages and percentages synchronized
without cast suppressions or fallback branches.
The `cuenv` build script generates `llms-full.txt` from `llms.txt` plus schema
files using a fallible `Result` path instead of build-script panic/unwrap
allowances.
CLI error rendering and early startup failures use direct redacted stderr
helpers so they do not depend on tracing or the event bus being healthy.
Executor dispatch lives in `crates/cuenv/src/commands/dispatch.rs`; path-local
and workspace-wide module evaluation helpers live in
`crates/cuenv/src/commands/module_evaluation.rs`; task CLI adapter construction
lives in `crates/cuenv/src/commands/handler/task_handler.rs` and hands off to
the structured `TaskExecutionRequest` owned by `crates/cuenv/src/commands/task/`;
named/label task selection resolution lives in
`crates/cuenv/src/commands/task/execution/selection.rs`, while task-list
rendering and format selection lives in
`crates/cuenv/src/commands/task/execution/listing.rs`; interactive picker
handoff lives in `crates/cuenv/src/commands/task/execution/picker.rs`, and
task help/detail rendering lives in
`crates/cuenv/src/commands/task/execution/help.rs`.
Startup, runtime selection, and early process routing stay in
`crates/cuenv/src/main.rs`; command paths that bypass the executor are isolated in
`crates/cuenv/src/sync_dispatch.rs` and `crates/cuenv/src/async_dispatch.rs`.
The CLI startup path installs the rustls ring provider before HTTP clients are
created and treats an already-installed provider as acceptable instead of
requiring a crate-wide `expect_used` allowance. Event tracing setup consumes
`TracingConfig` into owned local fields so startup does not need a
pass-by-value lint suppression.
The internal event coordinator keeps the client registry to the active message
sender only; registration metadata is logged at connect time rather than stored
as unread per-client state.
Coordinator messages use length-prefixed JSON with explicit frame-size checks
before converting between header and allocation sizes, so oversized frames fail
without relying on module-wide cast truncation allowances.
Stale coordinator cleanup keeps the Unix `libc::kill` call inside a small
helper after process-name verification, avoiding a module-wide unsafe-code
allowance.
`crates/cuenv/src/performance.rs` exposes the opt-in performance registry,
guards, and macros directly. Its summary average uses a checked/saturating
operation-count divisor, so it does not need cast-truncation or module-level
dead-code allowances.
The CLI library root keeps the public module/re-export wiring only; broad lint
allowances have been removed so warning exceptions stay local to the module
that needs them. Task and hook string renderers treat writes into `String` as
infallible formatting paths without relying on crate-wide `expect()` allowance.
The async dispatcher owns direct async commands plus changeset/release output
envelopes; the hook supervisor process runs through
`crates/cuenv/src/hook_supervisor.rs` and OCI activation runs through
`crates/cuenv/src/oci_activate.rs`. Release command flows stay under
`crates/cuenv/src/commands/release.rs`; binary artifact orchestration lives in
`crates/cuenv/src/commands/release/binaries.rs`, and release-prepare analysis,
version application, git publication, and PR creation live in
`crates/cuenv/src/commands/release/prepare.rs`. Cargo manifest entry-point
handling lives in `crates/release/src/manifest.rs`; workspace package
discovery, version inheritance, and internal dependency lookup live in
`crates/release/src/manifest/packages.rs`, while version writes live in
`crates/release/src/manifest/updates.rs`. Release pipeline dispatch
stays in `crates/release/src/orchestrator.rs`; package artifact handling lives
in `crates/release/src/orchestrator/package.rs`, and backend publication lives
in `crates/release/src/orchestrator/publish.rs`. Archive/checksum primitives
for release binaries live in `crates/release/src/artifact.rs`. Conventional
commit parsing uses explicit gix commit-time ordering and `git-conventional`
component accessors so the release path does not need interop lint
suppressions. The release crate root relies on its normal warning policy
without broad derive-workaround allowances; warning suppressions belong with the
module that actually needs them. Task-list data construction stays in
`crates/cuenv/src/commands/task_list.rs`;
text, rich, tables, dashboard, and emoji renderers live in
`crates/cuenv/src/commands/task_list/formatters.rs`.
The rich TUI keeps event-driven task/output state in
`crates/cuenv/src/tui/state/activity.rs`, input-driven view state in
`crates/cuenv/src/tui/state/view.rs`, tree flattening/navigation in
`crates/cuenv/src/tui/state/tree.rs`, and uses `crates/cuenv/src/tui/state.rs`
as the coordinator that preserves the renderer-facing accessors and
event-application boundary. TUI elapsed-time display uses a shared saturating
millisecond conversion at that state boundary.
Sync command provider adapters live under
`crates/cuenv/src/commands/sync/providers/`; shared sync command
orchestration remains in `crates/cuenv/src/commands/sync/functions.rs`, and
codegen file check/write/diff behavior lives in
`crates/cuenv/src/commands/sync/functions/codegen.rs`. GitHub workflow
sync and non-matrix workflow-shape emission helpers live in
`crates/cuenv/src/commands/sync/functions/github.rs`; matrix workflow
expansion and artifact aggregation live in
`crates/cuenv/src/commands/sync/functions/github/matrix.rs`. Public sync and
task command adapter re-exports stay explicit without local unused-import lint
guards.
The lock provider keeps path/workspace orchestration in
`crates/cuenv/src/commands/sync/providers/lock.rs`, while multi-source tool
resolution, cache reuse, source template expansion, and provider registry
setup live in `crates/cuenv/src/commands/sync/providers/lock/tool_resolution.rs`;
template placeholders stay named constants so literal `{version}`, `{os}`, and
`{arch}` replacement does not need formatting-lint suppressions.
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
Runtime affected-task selection lives in `crates/ci/src/affected.rs`; external
project maps are generic over the caller's `BuildHasher`, so the public API does
not force or suppress default hashing.
CI execution and garbage collection are decomposed into explicit planning,
execution, reporting, cache-scan, sweep, and finalization helpers instead of
depending on broad complexity suppressions. The CI crate root keeps only the
temporary missing-docs allowance; parser, derive, and clippy warnings should be
handled at the module that owns them. CI task DAG execution and IR runner setup
live in `crates/ci/src/executor/task_execution.rs`. CI task tool download and
lockfile activation support lives in `crates/ci/src/executor/tools.rs`; CI
hook-backed environment assembly lives in `crates/ci/src/executor/hook_env.rs`,
keeping the orchestrator focused on pipeline scheduling. CI report writing,
provider notification, annotation resolution, and CI redaction setup live in
`crates/ci/src/executor/reporting.rs`; per-task environment precedence and
passthrough handling live in `crates/ci/src/executor/task_env.rs`. CI report
durations are computed with checked non-negative conversions and display
formatting uses duration helpers rather than lossy numeric casts. Live pipeline
progress percentages are calculated with bounded integer basis points in
`crates/ci/src/report/progress.rs` before formatting as `f32` percentages. CI
digest diff comparison remains in `crates/ci/src/diff.rs`; human diff
formatting lives in `crates/ci/src/diff/format.rs`.
Core tool activation schema and environment mutation rules live in
`crates/core/src/tools/activation.rs`; provider/cache/profile path discovery
for lockfile activation lives in `crates/core/src/tools/activation/path_index.rs`.
The default secret registry keeps a stable fallible `Result` API while
registering env, exec, and optional 1Password/Infisical resolvers without a
local `unnecessary_wraps` suppression.
Secret batch convenience APIs accept caller-owned maps generically over their
`BuildHasher`, while the object-safe resolver trait keeps its internal concrete
map boundary for provider implementations.
The exec resolver's JSON command shape stays private inside
`crates/secrets/src/resolvers/exec.rs`; callers configure it through
`schema.#ExecSecret` rather than a public Rust constructor.
`cuenv secrets setup` orchestration lives in
`crates/cuenv/src/commands/secrets.rs`; setup output stays on the redacted
print path and formats download sizes with deterministic integer math.

### cuenv-1password

1Password secret resolution auto-selects between HTTP mode via the WASM SDK and
CLI mode via the `op` command. Resolver mode selection, WASM client lifecycle,
and HTTP batch resolution live in `crates/1password/src/secrets/resolver.rs`;
WASM host-function imports, memory-offset conversion, and Unix-time conversion
live in `crates/1password/src/secrets/core.rs` with checked/saturating integer
boundaries.
CLI authentication preflight, signed-out bootstrap reads, and `op read`
execution live in `crates/1password/src/secrets/cli.rs`.

### cuenv-events

The canonical event path constructs typed `CuenvEvent` values and publishes
them through the process-wide sender. The compatibility tracing layer in
`crates/events/src/layer.rs` owns target filtering and dispatch; its visitor in
`crates/events/src/layer/visitor.rs` translates tracing fields into typed
events. Field extraction and category-specific event construction stay
separated so output, task, service, CI, command, interactive, and system events
do not share one monolithic conversion path. Exported `emit_*!` macro
definitions live in
`crates/events/src/macros.rs`, while crate-root hidden helpers remain available
for `$crate` expansion and redacted print helpers. Redacted print helpers write
through explicit stdout/stderr handles so command output stays in the
redaction boundary without raw print macros. The CLI renderer keeps all
category-specific output in `crates/events/src/renderers/cli/`, leaving the
renderer root focused on configuration, event consumption, and dispatch; the
renderer root owns explicit stdout/stderr writer helpers so category renderers
do not need raw print macros. The JSON renderer exposes a writer boundary so
JSON-line output can be tested without direct print macros. Spinner interleaved
output stays behind `indicatif::MultiProgress::println`, preserving progress-bar
frames without direct stderr print macros.

### cuenv-workspaces

Workspace management for monorepos.

**Features:**

- Detect and configure workspaces
- Package manager integration (bun, pnpm, yarn, cargo)
- Workspace-aware task execution

Core workspace data models live under `crates/workspaces/src/core/types/`:
workspace/member traversal in `workspace.rs`, package-manager metadata in
`package_manager.rs`, and dependency/lockfile shapes in `dependency.rs`.
`types.rs` re-exports the public API used by downstream crates.
Workspace detection stays in `crates/workspaces/src/detection.rs`, with command
parsing, filesystem lock/config scanning, and package-manager hints split into
`detection/command.rs`, `detection/filesystem.rs`, and
`detection/package_json.rs`.

Cargo lockfile parsing keeps workspace member discovery in
`crates/workspaces/src/parsers/rust/cargo/workspace.rs`, separate from
`Cargo.lock` package entry and `SourceId` conversion in
`crates/workspaces/src/parsers/rust/cargo.rs`.
Workspace error tests keep the crate `Result` alias coverage on a genuinely
fallible conversion, so they do not need module-level lint suppression.

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
each supported shell snippet. Hook-backed export wait progress writes through
an explicit stderr helper so prompt-time status rendering stays out of raw
print macros.

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
