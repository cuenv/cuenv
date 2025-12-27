package nix

import "github.com/cuenv/cuenv/schema"

// #NixFlake is a pre-configured hook that sources the Nix flake dev shell
// before executing tasks. This ensures all flake-provided tools are available.
//
// Usage:
//
// import "github.com/cuenv/cuenv/contrib/nix"
//
// hooks: onEnter: nix: nix.#NixFlake
#NixFlake: schema.#ExecHook & {
	order:     10
	propagate: false
	command:   "nix"
	args: ["print-dev-env"]
	source: true
	inputs: ["flake.nix", "flake.lock"]
}
