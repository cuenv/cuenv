package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

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
	providers: ["github"]
	contributors: [c.#Nix, c.#Cuenv, c.#Cachix]
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
