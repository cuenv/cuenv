package schema

// Tasks can be a single task or a group of tasks
#Tasks: #Task | #TaskGroup

#Task: {
	shell?: #Shell
	command!: string
	args?: [...string]

	dependencies?: [...string]
	inputs?: [...string]
	outputs?: [...string]
	description?: string
}

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: [...#Tasks] | {[string]: #Tasks}
