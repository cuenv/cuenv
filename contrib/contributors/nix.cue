package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix installs Nix via the Determinate Systems installer.
//
// Active when:
// - Project uses Nix runtime (detected via runtimeType condition)
//
// Injects tasks:
// - cuenv:contributor:nix.install: Installs Nix using Determinate Systems installer
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
		label:    "Install Nix"
		priority: 0
		script:   "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none"
		provider: github: {
			uses: "DeterminateSystems/nix-installer-action@v16"
			with: "extra-conf": "accept-flake-config = true"
		}
	}]
}
