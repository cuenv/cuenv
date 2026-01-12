package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-cuenv-homebrew"

// Use Homebrew to install cuenv in CI - no Nix required
config: ci: cuenv: {
	source:  "homebrew"
	version: "latest" // Version is ignored for homebrew
}

ci: pipelines: {
	build: {
		tasks: [_t.build]
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with cuenv installed via Homebrew"]
		inputs: ["env.cue"]
	}
}
