package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

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
	pipelines: {
		build: {
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with Nix and Cachix caching"]
		inputs: ["env.cue"]
	}
}
