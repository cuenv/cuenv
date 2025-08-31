package schema

// We support non-string types for constraints
// but when exported to the actual environment,
// these will always be strings.
#Environment: {
	[=~"^[A-Z][A-Z0-9_]*$"]: string | int | bool | #Secret
}

// #Env defines the structure for environment variable configuration
#Env: #Environment & {
	// Environment-specific overrides
	environment?: [string]: #Environment
}
