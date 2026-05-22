package schema

// Infisical REST API secret reference.
#InfisicalSecret: #Secret & {
	resolver:    "infisical"
	projectId:   string
	environment: string
	secretName:  string

	// Optional read controls
	secretPath?:              string | *"/"
	type?:                    "shared" | "personal" | *"shared"
	version?:                 int
	expandSecretReferences?: bool | *true
	includeImports?:         bool | *true

	// Optional Infisical API base URL. Defaults to INFISICAL_API_URL or
	// https://us.infisical.com at runtime.
	apiUrl?: string
}
