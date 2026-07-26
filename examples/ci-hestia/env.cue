package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-hestia"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	providers: ["github"]
	contributors: [c.#Nix, c.#Hestia]
	pipelines: {
		build: {
			provider: github: hestia: {}
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with Nix and Hestia"]
		inputs: ["env.cue"]
	}
}
