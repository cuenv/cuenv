package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-flakehub-cache"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	contributors: [c.#FlakeHubCache]
	provider: github: flakehubCache: {}
	pipelines: {
		build: {
			provider: github: permissions: "id-token": "write"
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with Nix and FlakeHub Cache"]
		inputs: ["env.cue"]
	}
}
