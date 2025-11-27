package schema

#Devenv: #ExecHook & {
	order:     10
	propagate: true
	command:   "devenv"
	args: ["print-dev-env"]
	source: true
	inputs: ["devenv.nix", "devenv.lock", "devenv.yaml"]
}

