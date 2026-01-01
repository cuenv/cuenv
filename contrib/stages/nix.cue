package stages

import "github.com/cuenv/cuenv/schema"

// #Nix installs Nix via the Determinate Systems installer.
//
// Active when:
// - Project uses a Nix or Devenv runtime, OR
// - Cuenv source mode requires Nix (git or nix)
//
// Contributes to Bootstrap phase with priority 0 (runs first).
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: contributors: [stages.#Nix]
#Nix: schema.#Contributor & {
	id: "nix"
	when: {
		// Active if project uses Nix or Devenv runtime, or cuenv source needs Nix
		runtimeType:  ["nix", "devenv"]
		cuenvSource?: ["git", "nix"]
	}
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
