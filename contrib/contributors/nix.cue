package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix installs upstream Nix.
//
// Active when:
// - Project uses Nix runtime (detected via runtimeType condition)
//
// Injects tasks:
// - cuenv:contributor:nix.install: Installs upstream Nix
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
		priority: 2
		script:   "curl --proto '=https' --tlsv1.2 -sSf -L https://releases.nixos.org/nix/nix-2.31.2/install | sh -s -- --no-daemon --no-channel-add"
		provider: github: {
			uses: "cachix/install-nix-action@v31"
			with: {
				install_url: "https://releases.nixos.org/nix/nix-2.31.2/install"
				extra_nix_config: "accept-flake-config = true"
			}
		}
	}]
}
