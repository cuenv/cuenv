package schema

// Part of an interpolated environment variable.
// Can be a literal string or a secret that needs runtime resolution.
#EnvPart: string | #Secret

// Interpolated environment variable (array of parts).
// Parts are concatenated at runtime after resolving any secrets.
#InterpolatedEnv: [...#EnvPart]

// Environment variable value with optional policies
// Closed to avoid ambiguity with direct #Secret usage in #EnvironmentVariable
#EnvironmentVariableWithPolicies: close({
	value: string | int | bool | #Secret | #InterpolatedEnv
	policies?: [...#Policy]
})

// Environment variable can be a simple value or a value with policies
#EnvironmentVariable: string | int | bool | #Secret | #InterpolatedEnv | #EnvironmentVariableWithPolicies

// We support non-string types for constraints
// but when exported to the actual environment,
// these will always be strings.
#Environment: close({
	[=~"^[A-Z0-9_]*$"]: #EnvironmentVariable
})

// #Env defines the structure for environment variable configuration
#Env: close({
	#Environment
	// Environment-specific overrides
	environment?: [string]: #Environment
})