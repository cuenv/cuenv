package contributors

import "github.com/cuenv/cuenv/schema"

// #Infisical validates Infisical REST API secret resolution credentials.
//
// Active when:
// - Pipeline environment contains Infisical secret references
//   (resolver="infisical")
//
// Injects tasks:
// - cuenv:contributor:infisical.setup: Validates Infisical auth env for secret resolution
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Infisical]
#Infisical: schema.#Contributor & {
	id: "infisical"
	when: secretsProvider: ["infisical"]
	tasks: [{
		id:        "infisical.setup"
		label:     "Setup Infisical"
		priority:  20
		shell:     false
		dependsOn: ["cuenv.setup"]
		command:   "cuenv secrets setup infisical"
		env: {
			INFISICAL_CLIENT_ID:         "${INFISICAL_CLIENT_ID}"
			INFISICAL_CLIENT_SECRET:     "${INFISICAL_CLIENT_SECRET}"
			INFISICAL_TOKEN:             "${INFISICAL_TOKEN}"
			INFISICAL_ORGANIZATION_SLUG: "${INFISICAL_ORGANIZATION_SLUG}"
		}
	}]
}
