package schema

#Config: close({
	// Task output format (for task execution output)
	outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

	// Command-specific configuration
	commands?: #CommandsConfig

	// CI-specific configuration
	ci?: #CIConfig
})

// Command-specific configuration
#CommandsConfig: close({
	// Task command configuration
	task?: #TaskCommandConfig
})

// Task command configuration
#TaskCommandConfig: close({
	// Task list configuration (for `cuenv task` without arguments)
	list?: #TaskListConfig
})

// Task list display configuration
#TaskListConfig: close({
	// Output format for task listing
	// - "text": Plain tree structure (default for non-TTY)
	// - "rich": Colored tree structure (default for TTY)
	// - "tables": Category-grouped bordered tables
	// - "dashboard": Status dashboard with cache indicators
	// - "emoji": Emoji-prefixed semantic categories
	format?: "text" | "rich" | "tables" | "dashboard" | "emoji"
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
