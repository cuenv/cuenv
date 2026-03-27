package contributors

import "github.com/cuenv/cuenv/schema"

// #Cachix sets up Cachix for Nix binary caching.
//
// Active when:
// - ci.provider.github.cachix is configured
//
// Injects tasks:
// - cuenv:contributor:cachix.setup: Sets up Cachix for binary caching
//
// This is a GitHub-specific contributor and uses cachix/cachix-action.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Cachix]
//	ci: provider: github: cachix: name: "my-cache"
#Cachix: schema.#Contributor & {
	id: "cachix"
	when: providerConfig: ["github.cachix"]
	tasks: [{
		id:        "cachix.setup"
		label:     "Setup Cachix"
		priority:  9
		dependsOn: ["nix.install"]
		provider: github: {
			uses: "cachix/cachix-action@v17"
			with: {
				name:      "${CACHIX_CACHE_NAME}"
				authToken: "${CACHIX_AUTH_TOKEN}"
			}
		}
	}]
}
