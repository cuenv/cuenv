package schema

#Tasks: {
	description: string | *"No description provided"
	#TaskGroup | #Task
}

#Task: {
	shell: string | *"bash"
	command!: string
	args?: [...string]

	dependencies?: [...string]
	inputs?: [...string]
	outputs?: [...string]
}

// TaskGroup uses structure to determine execution mode:
// - Array of tasks: Sequential execution (order preserved)
// - Object of named tasks: Parallel execution with dependencies
#TaskGroup: {
	// Sequential: array of tasks executed in order
	tasks: [...#Tasks] |
	// Parallel with dependencies: object of named tasks  
	{[string]: #Tasks}
}
