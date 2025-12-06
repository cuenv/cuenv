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

	// Command to execute. Required unless 'script' is provided.
	command?: string

	// Inline script to execute (alternative to command).
	// When script is provided, shell defaults to bash if not specified.
	// Supports multiline strings and shebang lines for polyglot scripts.
	// Example:
	//   script: """
	//       #!/bin/bash
	//       set -euo pipefail
	//       echo "Building..."
	//       cargo build --release
	//       """
	script?: string

	// Validation: exactly one of command or script must be provided
	_hasCommand: command != _|_
	_hasScript:  script != _|_
	_validTask:  true & ((_hasCommand & !_hasScript) | (!_hasCommand & _hasScript))

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

	// Dagger-specific configuration for running this task in a container
	dagger?: #DaggerConfig

	// Task parameter definitions for CLI arguments
	// Allows tasks to accept positional and named arguments:
	//   cuenv task import.youtube VIDEO_ID --quality 1080p
	params?: #TaskParams
}

// Task parameter definitions
#TaskParams: {
	// Positional arguments (order matters, consumed left-to-right)
	// Referenced in args as {{0}}, {{1}}, etc.
	positional?: [...#Param]
	// Named arguments are declared as direct fields (--flag style)
	// Referenced in args as {{name}} where name matches the field name
	// Example: thumbnailUrl: { description: "URL", required: false }
	[!~"^positional$"]: #Param
}

// Parameter definition for task arguments
#Param: {
	// Human-readable description shown in --help
	description?: string
	// Whether the argument must be provided (default: false)
	required?: bool | *false
	// Default value if not provided
	default?: string
	// Type hint for validation (default: "string")
	type?: "string" | "bool" | "int" | *"string"
	// Short flag (single character, e.g., "t" for -t)
	short?: =~"^[a-zA-Z]$"
}

// Accepted task inputs:
// - string: File path, directory, or glob pattern
// - #ProjectReference: Cross-project task outputs
// - #TaskOutput: Same-project task outputs
#Input: string | #ProjectReference | #TaskOutput

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

// Dagger-specific task configuration for containerized execution
#DaggerConfig: {
	// Base container image (e.g., "node:20-alpine", "rust:1.75-slim")
	// Required unless 'from' is specified
	image?: string

	// Use container from a previous task as base instead of an image.
	// The referenced task must have run and produced a container.
	// Example: from: "deps" continues from the "deps" task's container
	from?: string

	// Secrets to mount or expose as environment variables.
	// Secrets are resolved using cuenv's secret resolvers (exec, 1Password, etc.)
	// and securely passed to Dagger without exposing plaintext in logs.
	secrets?: [...#DaggerSecret]

	// Cache volumes to mount for persistent build caching.
	// Cache volumes persist across task runs and speed up builds.
	cache?: [...#DaggerCacheMount]
}

// Secret configuration for Dagger containers
#DaggerSecret: {
	// Name identifier for the secret in Dagger
	name: string

	// Mount secret as a file at this path (e.g., "/root/.npmrc")
	path?: string

	// Expose secret as an environment variable with this name
	envVar?: string

	// Secret resolver - uses existing cuenv secret types (#Secret, #OnePasswordRef, etc.)
	resolver: #Secret
}

// Cache volume mount configuration
#DaggerCacheMount: {
	// Path inside the container to mount the cache (e.g., "/root/.npm", "/root/.cargo/registry")
	path: string

	// Unique name for the cache volume. Volumes with the same name share data.
	name: string
}
