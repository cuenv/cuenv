package schema

#Config: close({
	// Task output format
	outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

	// Cache configuration
	cacheMode?:    "off" | "read" | "read-write" | "write"
	cacheEnabled?: bool

	// Security and debugging
	auditMode?:   bool
	traceOutput?: bool

	// Default environment settings
	defaultEnvironment?:  string
	defaultCapabilities?: [...string]

	// Task backend configuration
	backend?: #BackendConfig
})

// Backend configuration
#BackendConfig: close({
	// Which backend to use: "host", "dagger", "remote"
	type: "host" | "dagger" | "remote" | *"host"

	// Backend-specific options
	options?: #BackendOptions
})

// Backend-specific options
#BackendOptions: close({
	// Container image for Dagger backend
	image?: string

	// Platform hint for Dagger backend
	platform?: string

	// REAPI server endpoint for remote backend
	endpoint?: string

	// Instance name for multi-tenant REAPI servers
	instanceName?: string

	// Authentication configuration for remote backends
	auth?: #BackendAuth

	// Target platform for Nix toolchain (e.g., "x86_64-linux")
	// When set, fetches the Nix closure for this platform instead of the host.
	// This enables cross-platform remote execution (e.g., macOS -> Linux workers).
	targetPlatform?: string
})

// Authentication configuration for backend services
#BackendAuth: #BearerAuth | #BuildBuddyAuth | #MTlsAuth

// Bearer token authentication (Authorization: Bearer <token>)
#BearerAuth: close({
	type:  "bearer"
	token: string | #Secret
})

// BuildBuddy API key authentication (x-buildbuddy-api-key: <token>)
#BuildBuddyAuth: close({
	type:   "buildbuddy"
	apiKey: string | #Secret
})

// mTLS authentication
#MTlsAuth: close({
	type:     "mtls"
	certPath: string
	keyPath:  string
	caPath?:  string
})
