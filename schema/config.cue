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
	// Source for cuenv binary in CI
	// - "git": Build from git checkout (requires Nix)
	// - "nix": Install via Nix flake (auto-configures Cachix)
	// - "homebrew": Install via Homebrew tap (no Nix required)
	// - "release": Download pre-built binary from GitHub Releases (default)
	source?: "git" | "nix" | "homebrew" | *"release"

	// Version to install
	// - "self": Use current checkout (default, for git/nix source)
	// - "latest": Latest release (for release mode)
	// - "0.17.0": Specific version tag
	version?: string | *"self"
})
