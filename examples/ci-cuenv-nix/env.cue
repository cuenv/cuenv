package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-cuenv-nix"

// Build cuenv from the checked-out repository flake in CI.
config: ci: cuenv: {
	source:  "nix"
	version: "self"
}

ci: pipelines: {
	build: {
		tasks: [_t.build]
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with cuenv installed via Nix flake"]
		inputs: ["env.cue"]
	}
}
