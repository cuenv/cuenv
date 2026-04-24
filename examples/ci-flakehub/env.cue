package examples

import (
	"github.com/cuenv/cuenv/schema"
	"github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-flakehub"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "packages.x86_64-linux.default"
}

ci: {
	providers: ["github"]
	contributors: [
		contributors.#Nix,
		contributors.#CuenvNix,
		contributors.#FlakeHub,
	]
	pipelines: publish: {
		name: "Publish tags to FlakeHub"
		when: {
			tag: "v?[0-9]+.[0-9]+.[0-9]+*"
			manual: tag: {
				description: "The existing tag to publish to FlakeHub"
				required:    true
				type:        "string"
			}
		}
		provider: github: {
			checkout: {
				uses:               "actions/checkout@v6"
				persistCredentials: false
				ref:                "${{ (inputs.tag != null) && format('refs/tags/{0}', inputs.tag) || '' }}"
			}
			flakehub: {
				name:               "owner/flake"
				visibility:         "public"
				tag:                "${{ inputs.tag }}"
				includeOutputPaths: true
			}
			permissions: {
				"id-token": "write"
				contents:   "read"
			}
		}
		tasks: [_t.check]
	}
}

tasks: check: schema.#Task & {
	command: "nix"
	args: ["flake", "check"]
	inputs: ["flake.nix", "flake.lock"]
}
