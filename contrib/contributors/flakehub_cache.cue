package contributors

import "github.com/cuenv/cuenv/schema"

// #FlakeHubCache configures FlakeHub Cache for Nix builds on GitHub Actions.
//
// Active when:
// - ci.provider.github.flakehubCache is enabled
//
// Injects tasks:
// - cuenv:contributor:flakehub-cache.setup: Authenticates Nix with FlakeHub Cache
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: {
//		contributors: [contributors.#Nix, contributors.#FlakeHubCache]
//		provider: github: flakehubCache: true
//	}
#FlakeHubCache: schema.#Contributor & {
	id: "flakehub-cache"
	when: providerConfig: ["github.flakehubCache"]
	tasks: [{
		id:        "flakehub-cache.setup"
		label:     "Setup FlakeHub Cache"
		priority: 5
		dependsOn: ["nix.install"]
		provider: github: uses: "DeterminateSystems/flakehub-cache-action@main"
	}]
}
