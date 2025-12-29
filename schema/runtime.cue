package schema

// #Runtime declares where/how a task executes.
// Set at project level as the default, override per-task as needed.
//
// Use the specific runtime type directly:
//   runtime: #NixRuntime
//   runtime: #DevenvRuntime
//   runtime: #ContainerRuntime & {image: "node:20"}
//   runtime: #DaggerRuntime & {image: "rust:1.75"}
//   runtime: #OCIRuntime & {platforms: ["darwin-arm64"], images: [{image: "nginx:1.25-alpine", extract: [{path: "/usr/sbin/nginx"}]}]}
//   runtime: #ToolsRuntime & {platforms: ["darwin-arm64"], tools: {jq: "1.7.1"}}
#Runtime: #NixRuntime | #DevenvRuntime | #ContainerRuntime | #DaggerRuntime | #OCIRuntime | #ToolsRuntime

// #NixRuntime activates a Nix flake devShell
#NixRuntime: {
	type: "nix"
	// Flake reference (default: "." for local flake.nix)
	flake: string | *"."
	// Output attribute path (default: devShells.${system}.default)
	output?: string
}

// #DevenvRuntime activates a devenv shell
#DevenvRuntime: {
	type: "devenv"
	// Path to devenv config directory (default: ".")
	path: string | *"."
}

// #ContainerRuntime runs tasks in a container image
// For simple "run in this image" use cases
#ContainerRuntime: {
	type: "container"
	// Container image (e.g., "node:20-alpine", "rust:1.75-slim")
	image!: string
}

// #DaggerRuntime provides advanced container execution with orchestration features
// Use when you need: container chaining, secrets mounting, cache volumes
#DaggerRuntime: {
	type: "dagger"
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

// #OCIRuntime fetches binaries from OCI images.
// Provides hermetic binary management with content-addressed caching.
//
// Images require explicit `extract` paths to specify which binaries to extract.
//
// Example:
//   runtime: #OCIRuntime & {
//       platforms: ["darwin-arm64", "linux-x86_64"]
//       images: [
//           { image: "nginx:1.25-alpine", extract: [{ path: "/usr/sbin/nginx" }] },
//           { image: "busybox:latest", extract: [{ path: "/bin/sh", as: "busybox-sh" }] },
//       ]
//   }
#OCIRuntime: {
	type: "oci"
	// Platforms to resolve and lock (e.g., "darwin-arm64", "linux-x86_64")
	platforms!: [...string]
	// OCI images to fetch binaries from
	images!: [...#OCIImage]
	// Cache directory (defaults to ~/.cache/cuenv/oci)
	cacheDir?: string
}

// #OCIImage specifies an OCI image to extract binaries from.
// Images require explicit `extract` paths to specify which binaries to extract.
#OCIImage: {
	// Full image reference (e.g., "nginx:1.25-alpine", "gcr.io/distroless/static:latest")
	image!: string
	// Rename the extracted binary (when package name differs from binary name)
	as?: string
	// Extraction paths specifying which binaries to extract from the image
	extract?: [...#OCIExtract]
}

// #OCIExtract specifies a binary to extract from a container image
#OCIExtract: {
	// Path to the binary inside the container (e.g., "/usr/sbin/nginx")
	path!: string
	// Name to expose the binary as in PATH (defaults to filename from path)
	as?: string
}

// #OCIActivate is a pre-configured hook that fetches OCI binaries
// and adds them to PATH before executing tasks.
//
// The hook runs `cuenv runtime oci activate` which:
// 1. Reads `cuenv.lock` to find artifacts for the current platform
// 2. Pulls and extracts binaries (if not already cached)
// 3. Outputs `export PATH=...` to add binaries to PATH
//
// Usage:
//   hooks: onEnter: oci: #OCIActivate
#OCIActivate: #ExecHook & {
	order:     10
	propagate: false
	command:   "cuenv"
	args: ["runtime", "oci", "activate"]
	source: true
	inputs: ["cuenv.lock"]
}
