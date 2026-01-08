package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-cuenv-nix"

// Use Nix flake to install cuenv in CI with Cachix binary cache
config: ci: cuenv: {
	source:  "nix"
	version: "0.19.0" // Install specific version via nix profile
}

ci: pipelines: {
	build: {
		tasks: ["build"]
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with cuenv installed via Nix flake"]
		inputs: ["env.cue"]
	}
}
