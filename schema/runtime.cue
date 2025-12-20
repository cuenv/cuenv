package schema

// #Runtime declares where/how a task executes.
// Set at project level as the default, override per-task as needed.
//
// Runtime types:
//   - nix: Activate Nix devShell before execution
//   - devenv: Activate devenv shell before execution
//   - container: Simple container execution
//   - dagger: Advanced container with caching, secrets, chaining
#Runtime: #NixRuntime | #DevenvRuntime | #ContainerRuntime | #DaggerRuntime

// #NixRuntime activates a Nix flake devShell
#NixRuntime: close({
	nix: {
		// Flake reference (default: "." for local flake.nix)
		flake: string | *"."
		// Output attribute path (default: devShells.${system}.default)
		output?: string
	}
})

// #DevenvRuntime activates a devenv shell
#DevenvRuntime: close({
	devenv: {
		// Path to devenv config directory (default: ".")
		path: string | *"."
	}
})

// #ContainerRuntime runs tasks in a container image
// For simple "run in this image" use cases
#ContainerRuntime: close({
	container: {
		// Container image (e.g., "node:20-alpine", "rust:1.75-slim")
		image!: string
	}
})

// #DaggerRuntime provides advanced container execution with orchestration features
// Use when you need: container chaining, secrets mounting, cache volumes
#DaggerRuntime: close({
	dagger: {
		// Base container image (required unless 'from' is specified)
		image?: string

		// Use container from a previous task as base instead of an image.
		// The referenced task must have run and produced a container.
		// Example: from: "deps" continues from the "deps" task's container
		from?: string

		// Secrets to mount or expose as environment variables.
		// Secrets are resolved using cuenv's secret resolvers and
		// securely passed to Dagger without exposing plaintext in logs.
		secrets?: [...#DaggerSecret]

		// Cache volumes to mount for persistent build caching.
		// Cache volumes persist across task runs and speed up builds.
		cache?: [...#DaggerCacheMount]
	}
})
