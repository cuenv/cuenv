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
	// Cross-project inputs from tasks in other projects (monorepo-only)
	externalInputs?: [...#ExternalInput]

	description?: string
}

// External input reference to another project's task within the same Git root
#ExternalInput: {
	// Path to external project root. May be absolute-from-repo-root (prefix "/")
	// or relative to the env.cue declaring this dependency.
	project: string
	// Name of the external task in that project
	task: string
	// Explicit selection and mapping of outputs to this task's hermetic workspace
	map: [...#Mapping]
}

#Mapping: {
	// Path of a declared output (file or directory) from the external task,
	// relative to the external project's root. Directories map recursively.
	from: string
	// Destination path inside the dependent task's hermetic workspace where the
	// selected file/dir will be materialized. Must be unique per mapping.
	to: string
}

// Notes:
// - 'from' values must be among the external task's declared outputs
// - Directories in 'from' map recursively
// - Each 'to' destination must be unique; collisions are disallowed
// - External tasks run with their own environment; no env injection from dependents

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: [...#Tasks] | {[string]: #Tasks}
