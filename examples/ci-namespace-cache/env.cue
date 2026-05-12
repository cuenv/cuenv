package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-namespace-cache"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	contributors: [c.#NamespaceCache]
	provider: github: namespaceCache: {}
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
		args: ["Building with Nix and Namespace cache"]
		inputs: ["env.cue"]
	}
}
