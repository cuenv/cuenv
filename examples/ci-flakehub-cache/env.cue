package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-flakehub-cache"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	provider: github: flakehubCache: true
	pipelines: {
		build: {
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: build: schema.#Task & {
	command: "echo"
	args: ["Building with Nix and FlakeHub Cache"]
	inputs: ["env.cue"]
}
