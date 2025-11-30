package schema

#Config: {
	// Task output format
	outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

	// Backend configuration for task execution
	backend?: #BackendConfig
}

// Backend configuration for task execution
#BackendConfig: {
	// Which backend to use by default for tasks
	// "host" runs tasks directly on the host machine (default)
	// "dagger" runs tasks inside Dagger containers
	type: *"host" | "dagger"

	// Backend-specific default options (for Dagger backend)
	options?: #BackendOptions
}

// Backend-specific options
#BackendOptions: {
	// Container image for Dagger backend (e.g., "ubuntu:22.04")
	image?: string
	// Optional platform specification (e.g., "linux/amd64")
	platform?: string
}