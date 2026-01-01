package stages

import "github.com/cuenv/cuenv/schema"

// #OnePassword sets up 1Password WASM SDK for secret resolution.
//
// Active when:
// - Pipeline environment contains 1Password secret references
//   (resolver="onepassword" or "op://" URIs)
//
// Contributes to Setup phase with priority 20.
// Depends on setup-cuenv to have run first.
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: contributors: [stages.#OnePassword]
#OnePassword: schema.#Contributor & {
	id: "1password"
	when: {
		// Active if environment uses 1Password secrets
		secretsProvider: ["onepassword"]
	}
	tasks: [{
		id:        "setup-1password"
		phase:     "setup"
		label:     "Setup 1Password"
		priority:  20
		shell:     false
		dependsOn: ["setup-cuenv"]
		command:   "cuenv secrets setup onepassword"
		env: OP_SERVICE_ACCOUNT_TOKEN: "${OP_SERVICE_ACCOUNT_TOKEN}"
	}]
}
