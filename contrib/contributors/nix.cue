package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix installs Determinate Nix.
//
// Active when:
// - Project uses Nix runtime (detected via runtimeType condition)
//
// Injects tasks:
// - cuenv:contributor:nix.install: Installs Determinate Nix
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Nix]
#Nix: schema.#Contributor & {
	id: "nix"
	tasks: [{
		id:       "nix.install"
		label:    "Install Determinate Nix"
		priority: 2
		script:   "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none"
		provider: github: {
			uses: "DeterminateSystems/determinate-nix-action@v3"
			with: "extra-conf": "accept-flake-config = true"
		}
	}]
}
