package contributors

import "github.com/cuenv/cuenv/schema"

// #NamespaceCache sets up Namespace nscloud-cache for Nix store caching.
//
// Active when:
// - ci.provider.github.namespaceCache is configured
//
// Injects tasks:
// - cuenv:contributor:namespaceCache.setup: Configures Namespace cache volumes
// - cuenv:contributor:namespaceCache.prepareDeterminateReceipt: Prepares restored Nix state for installation
// - cuenv:contributor:namespaceCache.cleanupDeterminateReceipt: Removes legacy receipt metadata before cache save
//
// This is a GitHub-specific contributor and requires a Namespace runner profile
// with a cache volume attached. It does not install Nix. The receipt cleanup
// tasks remove legacy installer metadata from restored volumes and before
// saving state. Their task IDs remain stable for existing configurations;
// upstream Nix does not create a receipt. Linux runners without systemd use a
// single-user installation, so restored root-owned state must belong to the
// runner before installation. Daemon installations retain their ownership.
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
			script: _removeDeterminateReceipt + "\n" + """
				if [ "$(uname -s)" = Linux ] && [ ! -e /run/systemd/system ] && [ -d /nix ] && [ ! -w /nix ]; then
					sudo chown -R "$(id -u):$(id -g)" /nix
				fi
				"""
		},
		{
			id:       "namespaceCache.cleanupDeterminateReceipt"
			label:    "Prune legacy Nix receipt"
			priority: 3
			script:   _removeDeterminateReceipt
		},
	]
}
