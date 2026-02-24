package schema

// Infisical secret reference.
//
// `path` is the full secret path including key, for example:
//   /team/service/API_KEY
#InfisicalSecret: #Secret & {
	resolver: "infisical"
	path:     string

	// Optional per-secret environment override.
	environment?: string

	// Optional per-secret project override.
	projectId?: string
}

// Infisical defaults for cuenv config.
#InfisicalConfig: close({
	// Default environment for Infisical secrets when not set on the secret.
	defaultEnvironment?: string

	// Default project ID for Infisical secrets.
	projectId?: string

	// If true, relative secret paths are resolved from the instance filesystem path.
	inheritPath?: bool | *false

	// Optional string replacements to apply to inherited paths.
	pathReplace?: [string]: string
})
