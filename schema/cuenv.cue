package schema

#Cuenv: {
	name?: string
	config?: #Config
	env?: #Env
	hooks?: #Hooks
	workspaces?: #Workspaces
	_ci?: #CI
	release?: #Release
	tasks?: [string]: #Tasks
}

#Workspaces: [string]: #WorkspaceConfig

#WorkspaceConfig: {
	enabled: bool | *true
	package_manager?: "npm" | "pnpm" | "yarn" | "yarn-classic" | "bun" | "cargo"
	root?: string

	// Workspace lifecycle hooks
	hooks?: #WorkspaceHooks

	// Task matchers for discovery-based generators (e.g., projen)
	generators?: [string]: #TaskMatcher
}

// Workspace lifecycle hooks for pre/post install
#WorkspaceHooks: {
	// Tasks or references to run before workspace install
	beforeInstall?: [...(#Task | #TaskRef)]
	// Tasks or references to run after workspace install
	afterInstall?: [...(#Task | #TaskRef)]
}

// Reference a task from another env.cue project by its name property
#TaskRef: {
	// Format: "#project-name:task-name" where project-name is the `name` field in env.cue
	// Example: "#projen-generator:bun.install"
	ref: =~"^#[a-zA-Z0-9._-]+:[a-zA-Z0-9._-]+$"
}

// Match tasks across workspace by metadata for discovery-based execution
#TaskMatcher: {
	// Limit to specific workspaces (by name)
	workspaces?: [...string]

	// Match tasks with these labels (all must match)
	labels?: [...string]

	// Match tasks whose command matches this value
	command?: string

	// Match tasks whose args contain specific patterns
	args?: [...#ArgMatcher]

	// Run matched tasks in parallel (default: true)
	parallel: bool | *true
}

// Pattern matcher for task arguments
#ArgMatcher: {
	// Match if any arg contains this substring
	contains?: string
	// Match if any arg matches this regex pattern
	matches?: string
}
