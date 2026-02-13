# Task Output References: stdout/stderr/exitCode

## Context

Users need to pass runtime outputs (stdout, stderr, exit code) from one task to another. The naive CUE syntax (`one[0].stdout`) doesn't work today because CUE evaluates eagerly — references resolve to default values at definition time, before any task runs. We need a mechanism that preserves CUE's structural reference syntax while deferring actual resolution to the Rust executor at runtime.

## Design

### Core Idea

Add `stdout`, `stderr`, and `exitCode` fields to `#Task` that evaluate to a **typed struct** (`#TaskOutputRef`), not a string. CUE resolves references like `tasks.tmpdir.stdout` to this struct at eval time. The Rust executor recognizes these structs and resolves them to actual values after upstream tasks complete.

### CUE Schema Changes

**File: `schema/tasks.cue`**

> **Implementation note**: Field names use regular (non-hidden) names (`cuenvOutputRef`, `cuenvTask`,
> `cuenvOutput`) rather than `_`-prefixed hidden fields. CUE's `_` prefix makes fields invisible to
> `v.Fields()` iteration in Go, which means the Go bridge's `buildValueClean()` would exclude them
> from JSON output. Regular field names serialize normally.

```cue
#TaskOutputRef: {
	cuenvOutputRef: true
	cuenvTask:      string
	cuenvOutput:    "stdout" | "stderr" | "exitCode"
}

#Task: {
	// Injected by Go bridge via FillPath; default empty string for standalone CUE usage
	_name: string | *""

	stdout:   #TaskOutputRef & { cuenvTask: _name, cuenvOutput: "stdout" }
	stderr:   #TaskOutputRef & { cuenvTask: _name, cuenvOutput: "stderr" }
	exitCode: #TaskOutputRef & { cuenvTask: _name, cuenvOutput: "exitCode" }

	args?: [...(string | #TaskOutputRef)]
	env?:  [string]: #EnvironmentVariable | #TaskOutputRef
	// ... existing fields unchanged
}
```

### Usage Examples

**Named tasks:**
```cue
tasks: {
	tmpdir: { command: "mktemp", args: ["-d"] }
	work: {
		command: "ls"
		env: {
			TEMP_DIR: tasks.tmpdir.stdout
		}
	}
}
```

**Sequences:**
```cue
tasks: {
	pipeline: [
		{ command: "mktemp", args: ["-d"] },
		{ command: "echo", args: [pipeline[0].stdout] },
	]
}
```

**Direct in args (no env indirection):**
```cue
tasks: {
	tmpdir: { command: "mktemp", args: ["-d"] }
	work: {
		command: "echo"
		args: [tasks.tmpdir.stdout]
	}
}
```

### Processing Pipeline

The implementation uses a **two-phase placeholder approach** instead of extending `Task.args` to a union type. This avoids breaking the existing `Vec<String>` API.

1. **CUE eval time**: `tasks.tmpdir.stdout` → `{ cuenvOutputRef: true, cuenvTask: "tmpdir", cuenvOutput: "stdout" }`
2. **`process_output_refs` (JSON pre-processing)**: Ref objects replaced with `"cuenv:ref:tmpdir:stdout"` placeholder strings; `stdout`/`stderr`/`exitCode` fields stripped from task objects; dependency pairs collected
3. **Task deserialization**: Sees plain strings in `args`/`env` — no API change needed
4. **FQDN rewriting (workspace layer)**: Bare names transformed to FQDNs for multi-project support
5. **Graph building**: `add_output_ref_deps()` creates petgraph edges from collected dependency pairs
6. **Execution**: `OutputRefResolver` replaces placeholder strings with actual values before spawning

### Rust Types

**File: `crates/core/src/tasks/output_refs.rs`**

```rust
pub struct TaskOutputRef {
    pub task: String,
    pub output: TaskOutputField,
}

pub enum TaskOutputField {
    Stdout,
    Stderr,
    ExitCode,
}

pub struct OutputRefResolver<'a> {
    pub task_name: &'a str,
    pub results: &'a HashMap<String, TaskResult>,
}
```

### Auto-dependency Inference

- `process_output_refs()` collects `(from_task, to_task)` pairs during JSON pre-processing
- `build_global_tasks()` converts bare names to FQDNs
- `add_output_ref_deps()` creates petgraph edges after `build_for_task()`
- No explicit `dependsOn` needed when using output refs

### Runtime Resolution

- **Graph path** (`execute_graph`): `results_map: HashMap<String, TaskResult>` populated between parallel groups
- **Sequence path** (`execute_sequential`): `seq_results` map populated between steps
- Both paths use `OutputRefResolver::resolve()` before spawning
- Whitespace auto-trimmed on stdout/stderr
- `exitCode` rejected in string context (args/env) with clear error message

### Caching

- `TaskOutputRef` values are runtime-resolved, so they cannot contribute to cache keys at definition time
- The upstream task's cache key transitively covers this — if the upstream output changes, its cache key changes, which invalidates the downstream task's inputs

### Go Bridge Changes

**File: `crates/cuengine/bridge.go`**

- `injectTaskNames()` walks the `tasks` struct and fills `_name` via `FillPath` + `cue.Hid("_name", "_")`
- Handles named tasks (`"build"`), group children (`"check.lint"`), sequence items (`"pipeline[0]"`)
- Called after `ctx.BuildInstance(inst)` but before JSON serialization

## Files Modified

| File | Change |
|------|--------|
| `schema/tasks.cue` | `#TaskOutputRef`, `stdout`/`stderr`/`exitCode` on `#Task`, widened `args`/`env` |
| `crates/cuengine/bridge.go` | `injectTaskNames()` + recursive walker + helpers |
| `crates/core/src/tasks/output_refs.rs` | NEW: `TaskOutputRef`, `OutputRefResolver`, `process_output_refs()`, 28 tests |
| `crates/core/src/tasks/mod.rs` | Module + re-exports |
| `crates/core/src/module.rs` | `Instance.output_ref_deps`, `process_output_refs()` in `from_raw()` |
| `crates/core/src/tasks/graph.rs` | `add_output_ref_deps()` with petgraph edges |
| `crates/core/src/tasks/executor.rs` | `results_map` in `execute_graph`, `seq_results` in `execute_sequential` |
| `crates/task-graph/src/graph.rs` | `get_task_mut()` |
| `crates/cuenv/src/commands/task/workspace.rs` | `GlobalTasksResult`, `rewrite_output_ref_placeholders()` |
| `crates/cuenv/src/commands/task/mod.rs` | Wiring: collect deps, pass to graph |

## Design Decisions

1. **Regular field names**: `cuenvOutputRef`, `cuenvTask`, `cuenvOutput` instead of `_`-prefixed hidden fields, because CUE's `_` prefix excludes fields from JSON serialization via the Go bridge.
2. **Placeholder strings**: Ref objects replaced with `"cuenv:ref:X:Y"` strings before deserialization, keeping `Task.args` as `Vec<String>` — no breaking API change across 90+ test sites.
3. **Trimming**: Always auto-trim stdout/stderr (strip surrounding whitespace). No opt-out needed.
4. **exitCode**: Integer-only — NOT valid in `args` or `env` (which expect strings). Available for CUE-level logic.
5. **Failure mode**: Fail early. If the upstream task exits non-zero, downstream tasks referencing its outputs do not run.
6. **FQDN rewriting**: Bare CUE names transformed to FQDNs at workspace layer, keeping core crate FQDN-agnostic.
7. **`_name` optional**: `_name: string | *""` so CUE evaluation succeeds without bridge injection. Empty default produces unparseable refs (safe).

## Known Limitations

1. **`cue.Hid("_name", "_")` package path**: Using `"_"` as the package path for hidden fields is unverified in integration tests. May need the actual CUE module package path.
2. **Cross-project refs**: Output refs only work within a single project. CUE's reference syntax is scoped to a single file/package.
3. **No E2E integration test**: The full CUE→Go→Rust pipeline is tested at each layer but not end-to-end.
