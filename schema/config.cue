package schema

#Config: {
	// Task output format (for task execution output)
	outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

	// Command-specific configuration
	commands?: #CommandsConfig

	// Legacy task backend selection. Prefer `runtime: #DaggerRuntime` once it
	// drives execution; today this field is what selects the Dagger backend.
	backend?: #BackendConfig

	// CI-specific configuration
	ci?: #CIConfig
}

// Task backend selection (legacy).
#BackendConfig: {
	// Backend that runs tasks. `cuenv task --backend` overrides it.
	type: "host" | "dagger" | *"host"
	options?: {
		// Default container image for Dagger tasks that set none.
		image?: string
	}
}

// Command-specific configuration
#CommandsConfig: {
	// Task command configuration
	task?: #TaskCommandConfig
}

// Task command configuration
#TaskCommandConfig: {
	// Task list configuration (for `cuenv task` without arguments)
	list?: #TaskListConfig
}

// Task list display configuration
#TaskListConfig: {
	// Output format for task listing
	// - "text": Plain tree structure (default for non-TTY)
	// - "rich": Colored tree structure (default for TTY)
	// - "tables": Category-grouped bordered tables
	// - "dashboard": Status dashboard with cache indicators
	// - "emoji": Emoji-prefixed semantic categories
	format?: "text" | "rich" | "tables" | "dashboard" | "emoji"
}

// CI-specific configuration
#CIConfig: {
	// Cuenv installation configuration for CI environments
	cuenv?: #CuenvConfig
}

// Configuration for cuenv installation in CI
#CuenvConfig: {
	// Source for cuenv binary in CI
	// - "native": Build using native Rust/Go toolchains (no Nix)
	// - "artifact": Use pre-built artifact from earlier CI job
	// - "git": Build from git checkout (requires Nix)
	// - "nix": Install via Nix flake (uses the configured Nix cache contributor)
	// - "homebrew": Install via Homebrew tap (no Nix required)
	// - "release": Download pre-built binary from GitHub Releases (default)
	source?: "native" | "artifact" | "git" | "nix" | "homebrew" | *"release"

	// Version to install
	// - "self": Use current checkout (default, for git/nix source)
	// - "latest": Latest release (for release mode)
	// - "0.17.0": Specific version tag
	version?: string | *"self"
}
