package schema

#NixFlake: #ExecHook & {
	order:     10
	propagate: false
	command:   "nix"
	args: ["print-dev-env"]
	source: true
	inputs: ["flake.nix", "flake.lock"]
}

