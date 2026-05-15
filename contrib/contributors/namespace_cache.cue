package contributors

import "github.com/cuenv/cuenv/schema"

// #NamespaceCache sets up Namespace nscloud-cache for Nix store caching.
//
// Active when:
// - ci.provider.github.namespaceCache is configured
//
// Injects tasks:
// - cuenv:contributor:namespaceCache.setup: Configures Namespace cache volumes
// - cuenv:contributor:namespaceCache.prepareDeterminateReceipt: Removes stale Determinate receipt metadata restored from cache
// - cuenv:contributor:namespaceCache.cleanupDeterminateReceipt: Removes new Determinate receipt metadata before cache save
//
// This is a GitHub-specific contributor and requires a Namespace runner profile
// with a cache volume attached. It does not install Nix. When paired with
// Determinate Nix, the prepare task removes restored /nix/receipt.json before
// Nix installation, and the cleanup task removes the new receipt before the
// Namespace cache action saves state.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#NamespaceCache]
//	ci: provider: github: namespaceCache: {}

let _removeDeterminateReceipt = """
	if [ -e /nix/receipt.json ]; then
		if command -v sudo >/dev/null 2>&1; then
			sudo rm -f /nix/receipt.json
		else
			rm -f /nix/receipt.json
		fi
	fi
	"""

#NamespaceCache: schema.#Contributor & {
	id: "namespaceCache"
	when: providerConfig: ["github.namespaceCache"]
	tasks: [
		{
			id:       "namespaceCache.setup"
			label:    "Setup Namespace Nix Cache"
			priority: 0
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				if:   "runner.os == 'Linux'"
				with: cache: "nix"
			}
		},
		{
			id:       "namespaceCache.prepareDeterminateReceipt"
			label:    "Prepare Namespace Nix Cache"
			priority: 1
			script:   _removeDeterminateReceipt
		},
		{
			id:       "namespaceCache.cleanupDeterminateReceipt"
			label:    "Prune Determinate Nix receipt"
			priority: 3
			script:   _removeDeterminateReceipt
		},
	]
}
