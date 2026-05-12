package contributors

import "github.com/cuenv/cuenv/schema"

// #NamespaceCache sets up Namespace nscloud-cache for Nix store caching.
//
// Active when:
// - ci.provider.github.namespaceCache is configured
//
// Injects tasks:
// - cuenv:contributor:namespaceCache.setup: Configures Namespace cache volumes
//
// This is a GitHub-specific contributor and requires a Namespace runner profile
// with a cache volume attached. It does not install Nix; use a runner image or
// profile that already provides Nix.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#NamespaceCache]
//	ci: provider: github: namespaceCache: {}
#NamespaceCache: schema.#Contributor & {
	id: "namespaceCache"
	when: providerConfig: ["github.namespaceCache"]
	tasks: [{
		id:       "namespaceCache.setup"
		label:    "Setup Namespace Nix Cache"
		priority: 0
		provider: github: {
			uses: "namespacelabs/nscloud-cache-action@v1"
			with: cache: "nix"
		}
	}]
}
