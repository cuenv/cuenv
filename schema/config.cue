package schema

#Config: close({
	// Task output format
	outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

	// CI-specific configuration
	ci?: #CIConfig
})

// CI-specific configuration
#CIConfig: close({
	// Cuenv installation configuration for CI environments
	cuenv?: #CuenvConfig
})

// Configuration for cuenv installation in CI
#CuenvConfig: close({
	// Source for cuenv binary
	// - "release": Download from GitHub Releases (default)
	// - "build": Build from source via nix build
	source?: "release" | *"release" | "build"
})
