package schema

#NixFlake: close({
	#ExecHook
	order:     10
	propagate: false
	command:   "nix"
	args: ["print-dev-env"]
	source: true
	inputs: ["flake.nix", "flake.lock"]
})

// Packages to install from package managers
// Currently supports Nix packages from nixpkgs
#Packages: close({
	// Nix packages from nixpkgs (e.g., "rustc", "cargo", "gcc")
	// These are fetched from cache.nixos.org and work cross-platform
	nix?: [...string]
})
