package schema

// =============================================================================
// Task API v2 - Explicit Types
// =============================================================================
//
// Users annotate tasks with their type to unlock specific semantics:
//   - #Task: Single command or script
//   - #TaskGroup: Parallel execution (all children run concurrently)
//   - #TaskList: Sequential execution (steps run in order)
//
// Example:
//   tasks: {
//       build: #Task & { command: "cargo build" }
//       check: #TaskGroup & {
//           parallel: {
//               lint: #Task & { command: "cargo clippy" }
//               test: #Task & { command: "cargo test" }
//           }
//       }
//   }

// Union of all task types - explicit typing required
#TaskNode: #Task | #TaskGroup | #TaskList

// =============================================================================
// Script Shell Configuration
// =============================================================================

// Interpreter for script-based tasks
#ScriptShell: "bash" | "sh" | "zsh" | "fish" | "powershell" | "pwsh" | "python" | "node" | "ruby" | "perl"

// Shell options (for bash-like shells)
#ShellOptions: {
	errexit?:  bool | *true  // -e: exit on error
	nounset?:  bool | *true  // -u: error on undefined vars
	pipefail?: bool | *true  // -o pipefail: fail on pipe errors
	xtrace?:   bool | *false // -x: debug/trace mode
}

// =============================================================================
// Single Executable Task
// =============================================================================

#Task: {
	// Task name - auto-injected by Go bridge based on task path
	_name?: string

	// Command-based execution
	command?: string
	args?: [...string]

	// Script-based execution (mutually exclusive with command)
	script?:       string
	scriptShell?:  #ScriptShell | *"bash"
	shellOptions?: #ShellOptions

	// Environment variables
	env?: [string]: #EnvironmentVariable

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

	// DEPRECATED: Use runtime: dagger: { ... } instead
	dagger?: #DaggerConfig
}

// =============================================================================
// Parallel Execution (Task Group)
// =============================================================================

#TaskGroup: {
	// Named children - all run concurrently
	parallel!: {[string]: #TaskNode}

	// Dependencies on other tasks
	dependsOn?: [...#TaskNode]

	// Limit concurrent executions (0 = unlimited)
	maxConcurrency?: int

	// Human-readable description
	description?: string
}

// =============================================================================
// Sequential Execution (Task List)
// =============================================================================

#TaskList: {
	// Ordered steps - run in sequence
	steps!: [...#TaskNode]

	// Dependencies on other tasks
	dependsOn?: [...#TaskNode]

	// Stop on first error (default: true)
	stopOnFirstError?: bool | *true

	// Human-readable description
	description?: string
}

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
