package schema

#Base: close({
	config?:     #Config
	env?:        #Env
	formatters?: #Formatters
})

#ProjectName: string & =~"^[a-zA-Z0-9._-]+$"

#Project: close({
	#Base
	name!:    #ProjectName
	runtime?: #Runtime
	hooks?:   #Hooks
	ci?:      #CI
	release?: #Release
	tasks?: [string]: #Tasks
	cube?:   #Cube
})

// Reference a task from another env.cue project by its name property.
#TaskRef: close({
	// Format: "#project-name:task-name" where project-name is the `name` field in env.cue
	// Example: "#projen-generator:bun.install"
	ref!: =~"^#[a-zA-Z0-9._-]+:[a-zA-Z0-9._-]+$"
})

// Match tasks across projects by metadata for discovery-based execution.
#TaskMatcher: close({
	// Match tasks with these labels (all must match)
	labels?: [...string]

	// Match tasks whose command matches this value
	command?: string

	// Match tasks whose args contain specific patterns
	args?: [...#ArgMatcher]

	// Run matched tasks in parallel (default: true)
	parallel: bool | *true
})

// Discovery-based hook step that expands a #TaskMatcher into tasks.
#MatchHook: close({
	// Optional stable name used for task naming/logging
	name?: string
	// Task matcher to select tasks across the workspace
	match!: #TaskMatcher
})

// Pattern matcher for task arguments
#ArgMatcher: close({
	// Match if any arg contains this substring
	contains?: string
	// Match if any arg matches this regex pattern
	matches?: string
})
