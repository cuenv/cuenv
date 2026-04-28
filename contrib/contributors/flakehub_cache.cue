package contributors

import "github.com/cuenv/cuenv/schema"

// #FlakeHubCache sets up Determinate Nix and FlakeHub Cache.
//
// Active when:
// - ci.provider.github.flakehubCache is configured
//
// Injects tasks:
// - cuenv:contributor:nix.install: Installs Determinate Nix
// - cuenv:contributor:flakehubCache.setup: Configures FlakeHub Cache
//
// This is a GitHub-specific contributor and requires id-token: write
// permissions in the generated workflow.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#FlakeHubCache]
//	ci: provider: github: flakehubCache: {}
#FlakeHubCache: schema.#Contributor & {
	id: "flakehubCache"
	when: providerConfig: ["github.flakehubCache"]
	tasks: [
		{
			id:       "nix.install"
			label:    "Install Determinate Nix"
			priority: 0
			provider: github: {
				uses: "DeterminateSystems/determinate-nix-action@v3"
				with: "extra-conf": "accept-flake-config = true"
			}
		},
		{
			id:        "flakehubCache.setup"
			label:     "Setup FlakeHub Cache"
			priority:  9
			dependsOn: ["nix.install"]
			provider: github: {
				uses: "DeterminateSystems/flakehub-cache-action@v3"
				with: {
					"use-gha-cache":        "${FLAKEHUB_CACHE_USE_GHA_CACHE}"
					"flakehub-flake-name": "${FLAKEHUB_CACHE_FLAKE_NAME}"
				}
			}
		},
	]
}
