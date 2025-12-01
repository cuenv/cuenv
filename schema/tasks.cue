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

	// When true (default), task runs in an isolated hermetic directory with only
	// declared inputs available. When false, task runs directly in the workspace
	// root (if workspaces specified) or project root. Non-hermetic tasks are useful
	// for install commands that need to write to the real filesystem.
	hermetic?: bool | *true

	dependsOn?: [...string]
	// Inputs accepted:
	// - File paths relative to the env.cue root, e.g. "src/index.ts"
	// - Directories (recursively included), e.g. "src" or "src/lib"
	// - Glob patterns (first-class), e.g. "src/**/*.ts", "assets/**/*.{png,jpg}"
	// - Project references that pull outputs from another task in the repo
	// All inputs are resolved relative to the project root and are the ONLY files
	// made available inside the hermetic working directory when executing the task.
	inputs?: [...#Input]
	// Outputs accepted (same syntax as inputs): files, directories, and globs relative
	// to the project root. Only declared outputs are indexed and persisted to the
	// cache for later materialization. Writes to undeclared paths are allowed but
	// will be warned about and are not indexed.
	outputs?: [...string]
	// Consume cached outputs from other tasks in the same project.
	// The referenced task's outputs are materialized into this task's hermetic workspace.
	inputsFrom?: [...#TaskOutput]
	// Workspaces to mount/enable for this task
	workspaces?: [...string]

	description?: string
}

// Accepted task inputs
#Input: string | #ProjectReference

// Reference to another project's task within the same Git root
#ProjectReference: {
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

// Reference to another task's outputs within the same project
#TaskOutput: {
	// Name of the task whose cached outputs to consume (e.g. "docs.build")
	task: string
	// Optional explicit mapping of outputs. If omitted, all outputs are
	// materialized at their original paths in the hermetic workspace.
	map?: [...#Mapping]
}

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: [...#Tasks] | {[string]: #Tasks}
