package contributors

import "github.com/cuenv/cuenv/schema"

// #NamespaceCache sets up the Namespace persistent Nix store cache.
//
// Active when:
// - ci.provider.github.namespaceCache is true
//
// Injects tasks:
// - cuenv:contributor:namespace-cache.setup: Sets up Namespace Nix store caching
//
// This is a GitHub-specific contributor that uses namespacelabs/nscloud-cache-action.
// It is designed for workflows running on Namespace Cloud runners which support
// persistent cache volumes for zero-latency cache access.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#NamespaceCache]
//	ci: provider: github: namespaceCache: true
#NamespaceCache: schema.#Contributor & {
	id: "namespace-cache"
	when: providerConfig: ["github.namespaceCache"]
	tasks: [{
		id:        "namespace-cache.setup"
		label:     "Setup Namespace Cache"
		priority:  1
		dependsOn: ["nix.install"]
		provider: github: {
			uses: "namespacelabs/nscloud-cache-action@v1"
			with: cache: "nix"
		}
	}]
}
