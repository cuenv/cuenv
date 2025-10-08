package schema

// #Tasks can represent either a single task or a group of tasks.
// Use a single #Task when you have an isolated command to run.
// Use a #TaskGroup when you need to define multiple tasks that should be executed
// either sequentially (as an array) or in parallel with dependencies (as an object).
// Choose the structure based on your workflow requirements:
//   - Single #Task: Simple, standalone execution.
//   - #TaskGroup: Complex workflows involving multiple tasks and dependencies.
#Tasks: #Task | #TaskGroup

#Task: {
	shell?: #Shell
	command!: string
	args?: [...string]
	env?: [string]: #EnvironmentVariable

	dependsOn?: [...string]
	// Inputs accepted:
	// - File paths relative to the env.cue root, e.g. "src/index.ts"
	// - Directories (recursively included), e.g. "src" or "src/lib"
	// - Glob patterns (first-class), e.g. "src/**/*.ts", "assets/**/*.{png,jpg}"
	// All inputs are resolved relative to the project root and are the ONLY files
	// made available inside the hermetic working directory when executing the task.
	inputs?: [...string]
	// Outputs accepted (same syntax as inputs): files, directories, and globs relative
	// to the project root. Only declared outputs are indexed and persisted to the
	// cache for later materialization. Writes to undeclared paths are allowed but
	// will be warned about and are not indexed.
	outputs?: [...string]
	description?: string
}

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: [...#Tasks] | {[string]: #Tasks}
