package schema

// A task can be a single task or a group of tasks
#Tasks: #Task | #TaskGroup

#Task: {
	shell: string | *"bash"
	command!: string
	args?: [...string]

	dependencies?: [...string]
	inputs?: [...string]
	outputs?: [...string]
	description?: string | *"No description provided"
}

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: [...#Tasks] | {[string]: #Tasks}
