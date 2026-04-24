package examples

import (
	"github.com/cuenv/cuenv/schema"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-namespace-cache"

// Nix runtime (required for Namespace Nix cache)
runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

// CI configuration with Namespace Nix store caching
ci: {
	contributors: [
		xContributors.#Nix,
		xContributors.#NamespaceCache,
	]
	provider: github: {
		runner:         "namespace-profile-my-project"
		namespaceCache: true
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
		args: ["Building with Nix and Namespace Nix caching"]
		inputs: ["env.cue"]
	}
}
