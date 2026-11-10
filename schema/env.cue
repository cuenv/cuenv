package schema

// Part of an interpolated environment variable.
// Can be a literal string or a secret that needs runtime resolution.
#EnvPart: string | #Secret

// Interpolated environment variable (array of parts).
// Parts are concatenated at runtime after resolving any secrets.
#InterpolatedEnv: [...#EnvPart]

// Environment variable value with optional policies
// Being a definition, it is closed, which keeps it distinct from a direct
// #Secret in #EnvironmentVariable
#EnvironmentVariableWithPolicies: {
	value: string | int | bool | #Secret | #InterpolatedEnv
	policies?: [...#Policy]
}

// Host environment variable passthrough for task environments.
// HOME cannot be passed through explicitly; set hermetic to false to inherit
// the caller's environment, including HOME.
// Forwards a host process env var into the task environment.
// The optional `name` field specifies which host var to read;
// when omitted the Rust executor uses the env map key.
#EnvPassthrough: {
	cuenvPassthrough: true
	name?:            string & !~"(?i)^HOME$"
}

// Environment variable can be a simple value or a value with policies
#EnvironmentVariable: string | int | bool | #Secret | #InterpolatedEnv | #EnvironmentVariableWithPolicies | #EnvPassthrough

// We support non-string types for constraints
// but when exported to the actual environment,
// these will always be strings.
#Environment: {
	[=~"^[A-Z_][A-Z0-9_]*$"]: #EnvironmentVariable
}

// #Env defines the structure for environment variable configuration
#Env: {
	#Environment

	// Environment-specific overrides
	environment?: [string]: #Environment

	// Overrides applied while the named context is active (see `contexts` on
	// #Base). A variable set under a context exists only there: it must not
	// also be set in the base or under `environment`, so it is withheld, and
	// its secret is never resolved, when no context that sets it is active.
	// Two contexts that can be active together must not set the same variable.
	context?: [string]: {
		#Environment

		// Environment-specific overrides within the context
		environment?: [string]: #Environment
	}
}
