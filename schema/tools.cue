package schema

// #ToolsRuntime provides multi-source tool management with platform overrides.
// Supports fetching tools from:
// - GitHub Releases (static binaries)
// - Nix flakes (complex toolchains)
// - OCI container images (custom distributions)
//
// Example:
//   runtime: #ToolsRuntime & {
//       platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
//       flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-unstable"
//       tools: {
//           // GitHub releases with platform overrides
//           jq: {
//               version: "1.7.1"
//               overrides: [
//                   {os: "darwin", arch: "arm64", source: #GitHub & {repo: "jqlang/jq", asset: "jq-macos-arm64"}},
//                   {os: "darwin", arch: "x86_64", source: #GitHub & {repo: "jqlang/jq", asset: "jq-macos-amd64"}},
//                   {os: "linux", arch: "x86_64", source: #GitHub & {repo: "jqlang/jq", asset: "jq-linux-amd64"}},
//               ]
//           }
//
//           // Nix package
//           rust: {version: "1.83.0", source: #Nix & {flake: "nixpkgs", package: "rustc"}}
//       }
//   }
#ToolsRuntime: {
	type: "tools"
	// Platforms to resolve and lock
	platforms!: [...#Platform]
	// Named Nix flake references for pinning
	flakes?: [string]: string
	// GitHub provider configuration
	github?: #GitHubProvider
	// Tool specifications (version string or full #Tool)
	tools!: [string]: string | #Tool
	// Cache directory (defaults to ~/.cache/cuenv/tools)
	cacheDir?: string
}

// GitHub provider configuration
#GitHubProvider: {
	// Authentication token (must use secret resolver)
	token?: #Secret
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
#Source: #Oci | #GitHub | #Nix | #Rustup

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
	// Tag prefix (prepended to version, defaults to "")
	tagPrefix: string | *""
	// Release tag override (if set, ignores tagPrefix and uses this template directly)
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

// #Rustup manages Rust toolchains via rustup
#Rustup: {
	type: "rustup"
	// Toolchain identifier (e.g., "stable", "1.83.0", "nightly-2024-01-01")
	toolchain!: string
	// Installation profile: minimal, default, complete
	profile: "minimal" | "default" | "complete" | *"default"
	// Additional components to install (e.g., "clippy", "rustfmt", "rust-src")
	components: [...string] | *[]
	// Additional targets to install (e.g., "x86_64-unknown-linux-gnu")
	targets: [...string] | *[]
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
