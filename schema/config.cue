package schema

#Config: {
	// Task output format
	outputFormat?: "tui" | "spinner" | "simple" | "tree"
	
	// Cache configuration
	cacheMode?: "off" | "read" | "read-write" | "write"
	cacheEnabled?: bool
	
	// Security and debugging
	auditMode?: bool
	traceOutput?: bool  // Chrome trace generation
	
	// Default environment settings
	defaultEnvironment?: string
	defaultCapabilities?: [...string]
}