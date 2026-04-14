package schema

// =============================================================================
// Container Image Output References
// =============================================================================
//
// Images produce `.ref` and `.digest` output references, resolved at runtime
// after the image is built. These can be consumed by tasks and other images:
//
//   tasks: {
//       deploy: {
//           dependsOn: [images.api]
//           env: IMAGE: images.api.ref
//       }
//   }

#ImageOutputRef: {
	cuenvOutputRef: true
	cuenvImage:     string
	cuenvOutput:    "ref" | "digest"
}

// =============================================================================
// Container Image Build Definition
// =============================================================================
//
// Declarative container image builds as first-class project artifacts.
// Images participate in the task DAG and can be built via `cuenv build`.
//
// Example:
//   images: {
//       api: #ContainerImage & {
//           context: "."
//           tags: ["latest", "v1.0.0"]
//           registry: "ghcr.io/myorg"
//           inputs: ["src/**", "Dockerfile"]
//       }
//   }

#ContainerImage: close({
	_cuenvPrefix: string | *""
	_cuenvSelf:   string | *""
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Type discriminator
	type: "image"

	// Runtime output references — resolved after build
	ref:    #ImageOutputRef & {cuenvImage: _name, cuenvOutput: "ref"}
	digest: #ImageOutputRef & {cuenvImage: _name, cuenvOutput: "digest"}

	// Build source
	context!:    string                  // Build context directory
	dockerfile?: string | *"Dockerfile" // Relative to context

	// Build configuration
	buildArgs?: [string]: string | #ImageOutputRef
	target?: string // Multi-stage build target

	// Tagging
	tags?: [...string]

	// Registry (omit for local-only builds)
	registry?:   string // e.g., "ghcr.io/cuenv"
	repository?: string // e.g., "cuenv/api" (defaults to image name)

	// Multi-platform
	platform?: [...string] // e.g., ["linux/amd64", "linux/arm64"]

	// DAG integration
	dependsOn?: [...(#TaskNode | #ContainerImage)]
	labels?: [...string]

	// Cache / inputs
	inputs?: [...#Input]

	// Human-readable description
	description?: string
})
