package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-cachix"

// Nix runtime (required for Cachix)
runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

// CI configuration with Cachix binary caching
ci: {
	provider: github: cachix: {
		name: "my-project-cache"
	}
	pipelines: [
		{
			name:  "build"
			tasks: ["build"]
			when: branch: "main"
		},
	]
}

tasks: {
	build: {
		command: "echo"
		args: ["Building with Nix and Cachix caching"]
		inputs: ["env.cue"]
	}
}
