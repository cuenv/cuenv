package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-cuenv-homebrew"

// Use Homebrew to install cuenv in CI - no Nix required
config: ci: cuenv: {
	source:  "homebrew"
	version: "latest" // Version is ignored for homebrew
}

ci: pipelines: [
	{
		name:  "build"
		tasks: ["build"]
	},
]

tasks: {
	build: {
		command: "echo"
		args: ["Building with cuenv installed via Homebrew"]
		inputs: ["env.cue"]
	}
}
