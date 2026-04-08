package schema

// =============================================================================
// Task API v2 - Explicit Types
// =============================================================================
//
// Users annotate tasks with their type to unlock specific semantics:
//   - #Task: Single command or script
//   - #TaskGroup: Parallel execution (all children run concurrently)
//   - #TaskSequence: Sequential execution (steps run in order)
//
// Example:
//   tasks: {
//       build: #Task & { command: "cargo build" }
//       check: #TaskGroup & {
//           type: "group"
//           lint: #Task & { command: "cargo clippy" }
//           test: #Task & { command: "cargo test" }
//       }
//       deploy: #TaskSequence & [
//           #Task & { command: "build" },
//           #Task & { command: "push" },
//       ]
//   }

// Union of all task types - explicit typing required
#TaskNode: #Task | #TaskGroup | #TaskSequence

// =============================================================================
// Task Output References
// =============================================================================
//
// Tasks produce stdout, stderr, and exitCode fields that evaluate to
// #TaskOutputRef structs. These can be referenced by other tasks:
//
//   tasks: {
//       tmpdir: { command: "mktemp", args: ["-d"] }
//       work: {
//           command: "ls"
//           env: TEMP_DIR: tasks.tmpdir.stdout
//       }
//   }
//
// Field names use no underscore prefix so they appear in JSON output.
// The Rust executor resolves these at runtime after upstream tasks complete.

#TaskOutputRef: {
	cuenvOutputRef: true
	cuenvTask:      string
	cuenvOutput:    "stdout" | "stderr" | "exitCode"
}

// =============================================================================
// Script Shell Configuration
// =============================================================================

// Interpreter for script-based tasks
#ScriptShell: "bash" | "sh" | "zsh" | "fish" | "nu" | "powershell" | "pwsh" | "python" | "node" | "ruby" | "perl"

// Shell options (for bash-like shells)
#ShellOptions: {
	errexit?:  bool | *true  // -e: exit on error
	nounset?:  bool | *true  // -u: error on undefined vars
	pipefail?: bool | *true  // -o pipefail: fail on pipe errors (bash/zsh only)
	xtrace?:   bool | *false // -x: debug/trace mode
}

// =============================================================================
// Task Cache Policy
// =============================================================================

#TaskCacheMode: "never" | "read" | "write" | "read-write"

#TaskCachePolicy: {
	// Cache mode (default: never)
	mode?: #TaskCacheMode | *"never"
	// Maximum age for cache reads, e.g. "30m", "1h", "infinite"
	maxAge?: string
}

// =============================================================================
// Single Executable Task
// =============================================================================

// Hidden helper fields used to derive fully-qualified task names from label aliases.
// Named tasks and group children get these from schema templates; sequence items
// still rely on the Go bridge because CUE does not yet support aliases on list
// elements.
#Task: {
	_cuenvPrefix: string | *""
	_cuenvSelf:   string | *""

	// Disallow 'type' field to prevent matching #TaskGroup pattern
	type?: _|_

	// Fully-qualified task name used by runtime output references.
	// For named tasks and group children this is derived in schema via label aliases.
	// The Go bridge fills sequence item names (pipeline[0], etc.) before JSON serialization.
	// The default empty string preserves evaluation when no name has been injected yet.
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Runtime output references - resolve to #TaskOutputRef structs.
	// Other tasks can reference these (e.g., tasks.tmpdir.stdout) to pass
	// runtime outputs between tasks. Resolved by Rust executor, not CUE.
	stdout: #TaskOutputRef & {cuenvTask: _name, cuenvOutput: "stdout"}
	stderr: #TaskOutputRef & {cuenvTask: _name, cuenvOutput: "stderr"}
	exitCode: #TaskOutputRef & {cuenvTask: _name, cuenvOutput: "exitCode"}

	// Command-based execution
	command?: string
	args?: [...(string | #TaskOutputRef)]

	// Script-based execution (mutually exclusive with command)
	script?:       string
	scriptShell?:  #ScriptShell | *"bash"
	shellOptions?: #ShellOptions

	// Environment variables
	env?: [string]: #EnvironmentVariable | #TaskOutputRef

	// Working directory override
	dir?: string

	// When true (default), task runs in an isolated hermetic directory with only
	// declared inputs available. When false, task runs directly in the workspace.
	hermetic?: bool | *true

	// Dependencies - reference other tasks directly for compile-time validation
	dependsOn?: [...#TaskNode]

	// Labels for task discovery via #TaskMatcher
	labels?: [...string]

	// Input files/patterns for caching and hermetic execution
	inputs?: [...#Input]

	// Output files/patterns for caching
	outputs?: [...string]

	// Per-task task-result cache policy
	cache?: #TaskCachePolicy

	// Human-readable description
	description?: string

	// Runtime override for this task
	runtime?: #Runtime

	// Task parameter definitions for CLI arguments
	params?: #TaskParams

	// Execution policies
	timeout?: string // e.g. "30m"
	retry?: {
		attempts: int | *3
		delay?:   string // e.g. "5s"
	}
	continueOnError?: bool | *false

	// Named regex captures extracted from task stdout/stderr after execution
	captures?: [string]: #TaskCapture

	// DEPRECATED: Use runtime: dagger: { ... } instead
	dagger?: #DaggerConfig
}

// =============================================================================
// Parallel Execution (Task Group)
// =============================================================================

#TaskGroup: {
	_cuenvPrefix: string | *""
	_cuenvSelf:   string | *""

	// Fully-qualified group path used to derive child task names.
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Type discriminator - must be "group"
	type: "group"

	// Dependencies on other tasks
	dependsOn?: [...#TaskNode]

	// Limit concurrent executions (0 = unlimited)
	maxConcurrency?: int

	// Human-readable description
	description?: string

	// Named children - all run concurrently (as direct fields).
	// Task and group children derive their fully-qualified names from the
	// current group's _name. Sequence children keep their name context in the
	// bridge, which can see list indexes.
	{[childName= !~"^(type|dependsOn|maxConcurrency|description)$"]: ((#Task | #TaskGroup) & {
		_cuenvPrefix: _name + "."
		_cuenvSelf:   childName
	}) | #TaskSequence
	}
}

// =============================================================================
// Sequential Execution (Task Sequence)
// =============================================================================

// A sequence is simply an ordered list of task nodes - run in order
#TaskSequence: [...#TaskNode]

// =============================================================================
// Task Parameters
// =============================================================================

#TaskParams: {
	// Positional arguments (order matters, consumed left-to-right)
	positional?: [...#Param]
	// Named arguments are declared as direct fields (--flag style)
	[!~"^positional$"]: #Param
}

#Param: {
	description?: string
	required?:    bool | *false
	default?:     string
	type?:        "string" | "bool" | "int" | *"string"
	short?:       =~"^[a-zA-Z]$"
}

// =============================================================================
// Task Inputs
// =============================================================================

// Accepted task inputs:
// - string: File path, directory, or glob pattern
// - #ProjectReference: Cross-project task outputs
// - #TaskOutput: Same-project task outputs
#Input: string | #ProjectReference | *#TaskOutput

// Reference to another project's task within the same Git root
#ProjectReference: close({
	project!: string
	task!:    string
	map!: [...#Mapping]
})

#Mapping: close({
	from!: string
	to!:   string
})

// Reference to another task's outputs within the same project
#TaskOutput: close({
	task!: string
	map?: [...#Mapping]
})

// =============================================================================
// Task Captures (Regex Extraction from Output)
// =============================================================================

// Regex capture from task output streams.
// The first capture group's match becomes the named value.
#TaskCapture: close({
	pattern!: string
	source?:  "stdout" | "stderr" | *"stdout"
})

// Reference to a captured value, resolved at runtime by the executor.
// Usage in annotations:
//   annotations: "Preview URL": schema.#TaskCaptureRef & {
//       cuenvTask: "deploy.preview", cuenvCapture: "previewUrl"
//   }
#TaskCaptureRef: {
	cuenvCaptureRef: true
	cuenvTask!:      string
	cuenvCapture!:   string
}

// =============================================================================
// Dagger Configuration (Containerized Execution)
// =============================================================================

#DaggerConfig: {
	image?: string
	from?:  string
	secrets?: [...#DaggerSecret]
	cache?: [...#DaggerCacheMount]
}

#DaggerSecret: {
	name:     string
	path?:    string
	envVar?:  string
	resolver: #Secret
}

#DaggerCacheMount: {
	path: string
	name: string
}
