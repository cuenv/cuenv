package schema

// Environment variable value with optional policies
// Closed to avoid ambiguity with direct #Secret usage in #EnvironmentVariable
#EnvironmentVariableWithPolicies: close({
	value: string | int | bool | #Secret
	policies?: [...#Policy]
})

// Environment variable can be a simple value or a value with policies
#EnvironmentVariable: string | int | bool | #Secret | #EnvironmentVariableWithPolicies

// We support non-string types for constraints
// but when exported to the actual environment,
// these will always be strings.
#Environment: close({
	[=~"^[A-Z][A-Z0-9_]*$"]: #EnvironmentVariable
})

// #Env defines the structure for environment variable configuration
#Env: close({
	#Environment
	// Environment-specific overrides
	environment?: [string]: #Environment
})