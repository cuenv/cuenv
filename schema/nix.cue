package schema

#NixFlake: #ExecHook & {
	command: "nix"
	args: [ "print-dev-env" ]
	source: true
	inputs: ["flake.nix", "flake.lock"]
}

