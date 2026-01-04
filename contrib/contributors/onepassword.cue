package contributors

import "github.com/cuenv/cuenv/schema"

// #OnePassword sets up 1Password WASM SDK for secret resolution.
//
// Active when:
// - Pipeline environment contains 1Password secret references
//   (resolver="onepassword" or "op://" URIs)
//
// Injects tasks:
// - cuenv:contributor:1password.setup: Sets up 1Password SDK for secret resolution
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#OnePassword]
#OnePassword: schema.#Contributor & {
	id: "1password"
	when: secretsProvider: ["onepassword"]
	tasks: [{
		id:        "1password.setup"
		label:     "Setup 1Password"
		priority:  20
		shell:     false
		dependsOn: ["cuenv.setup"]
		command:   "cuenv secrets setup onepassword"
		env: OP_SERVICE_ACCOUNT_TOKEN: "${OP_SERVICE_ACCOUNT_TOKEN}"
	}]
}
