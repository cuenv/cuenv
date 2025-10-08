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
	inputs?: [...string]
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
