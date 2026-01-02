package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix installs Nix via the Determinate Systems installer.
//
// Contributes to Bootstrap phase with priority 0 (runs first).
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Nix]
#Nix: schema.#Contributor & {
	id: "nix"
	tasks: [{
		id:       "install-nix"
		phase:    "bootstrap"
		label:    "Install Nix"
		priority: 0
		shell:    true
		command:  "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none"
		provider: github: {
			uses: "DeterminateSystems/nix-installer-action@v16"
			with: "extra-conf": "accept-flake-config = true"
		}
	}]
}
