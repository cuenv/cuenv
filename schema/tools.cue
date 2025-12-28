package schema

// #ToolsRuntime provides multi-source tool management with platform overrides.
// Replaces #OCIRuntime with a more ergonomic API supporting:
// - Homebrew bottles (default, 80% case)
// - OCI container images
// - GitHub Releases
// - Nix flakes
//
// Example:
//   runtime: #ToolsRuntime & {
//       platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
//       tools: {
//           // Simple Homebrew (most common)
//           jq: "1.7.1"
//           yq: "4.44.6"
//
//           // Homebrew with formula override
//           go: {version: "1.24.0", source: #Homebrew & {formula: "go@1.24"}}
//
//           // Platform-specific sources
//           bun: {
//               version: "1.3.5"
//               source: #Homebrew
//               overrides: [
//                   {os: "linux", source: #Oci & {image: "oven/bun:1.3.5", path: "/usr/local/bin/bun"}},
//               ]
//           }
//       }
//   }
#ToolsRuntime: {
	type: "tools"
	// Platforms to resolve and lock
	platforms!: [...#Platform]
	// Named Nix flake references for pinning
	flakes?: [string]: string
	// Tool specifications (version string or full #Tool)
	tools!: [string]: string | #Tool
	// Cache directory (defaults to ~/.cache/cuenv/tools)
	cacheDir?: string
}

// Supported platforms
#Platform: "darwin-arm64" | "darwin-x86_64" | "linux-x86_64" | "linux-arm64"

// OS for platform matching
#OS: "darwin" | "linux"

// Architecture for platform matching
#Arch: "arm64" | "x86_64"

// #Tool is a full tool specification with source and overrides
#Tool: {
	// Version string (e.g., "1.7.1", "latest")
	version!: string
	// Rename the binary in PATH
	as?: string
	// Default source for all platforms
	source?: #Source
	// Platform-specific source overrides
	overrides?: [...#Override]
}

// #Override specifies a source for specific platforms
#Override: {
	// Match by OS (darwin, linux)
	os?: #OS
	// Match by architecture (arm64, x86_64)
	arch?: #Arch
	// Source for matching platforms
	source!: #Source
}

// #Source is a union of all supported tool sources
#Source: #Homebrew | #Oci | #GitHub | #Nix

// #Homebrew fetches from Homebrew bottles (ghcr.io/homebrew)
#Homebrew: {
	type: "homebrew"
	// Formula name (defaults to tool name)
	formula?: string
}

// #Oci extracts binaries from OCI container images
#Oci: {
	type: "oci"
	// Image reference with optional {version}, {os}, {arch} templates
	image!: string
	// Path to binary inside the container
	path!: string
}

// #GitHub downloads from GitHub Releases
#GitHub: {
	type: "github"
	// Repository (owner/repo)
	repo!: string
	// Release tag (defaults to "v{version}")
	tag?: string
	// Asset name with optional {version}, {os}, {arch} templates
	asset!: string
	// Path to binary within archive (if archived)
	path?: string
}

// #Nix builds from a Nix flake
#Nix: {
	type: "nix"
	// Named flake reference (key in runtime.flakes)
	flake!: string
	// Package attribute (e.g., "jq", "python3")
	package!: string
	// Output path if binary can't be auto-detected
	output?: string
}

// #ToolsActivate is a pre-configured hook that downloads tools
// and adds them to PATH before executing tasks.
//
// The hook runs `cuenv tools activate` which:
// 1. Reads `cuenv.lock` to find tools for the current platform
// 2. Downloads and extracts binaries (if not already cached)
// 3. Outputs `export PATH=...` to add binaries to PATH
//
// Usage:
//   hooks: onEnter: tools: #ToolsActivate
#ToolsActivate: #ExecHook & {
	order:     10
	propagate: false
	command:   "cuenv"
	args: ["tools", "activate"]
	source: true
	inputs: ["cuenv.lock"]
}
