package schema

// =============================================================================
// Base execution types
// =============================================================================
//
// Tasks and service entrypoints share two fundamental shapes: a structured
// command+args invocation, or an inline script. Extracting them as base types
// lets #Service.entrypoint accept the same literal shape users already know
// from tasks, and lets a service reuse an existing task definition by
// reference.

// Command-based execution: a program plus its arguments.
#Command: {
	command: string
	args?: [...(string | #TaskOutputRef)]
}

// Script-based execution: an inline script interpreted by a shell.
#Script: {
	script:        string
	scriptShell?:  #ScriptShell | *"bash"
	shellOptions?: #ShellOptions
}
