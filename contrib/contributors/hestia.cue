package contributors

import "github.com/cuenv/cuenv/schema"

// #Hestia configures Hestia as a Nix binary cache backed by the GitHub
// Actions cache API.
//
// Active when:
// - ci.provider.github.hestia is configured
//
// The action runs after Nix is installed, serves cached paths through a local
// substituter, and uploads paths built by the job from its post step. cuenv
// pins both the action implementation and downloaded Hestia binary. Generated
// repositories also receive a dedicated daily cache GC workflow.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: providers: ["github"]
//	ci: contributors: [contributors.#Nix, contributors.#Hestia]
//	ci: provider: github: hestia: {}
#Hestia: schema.#Contributor & {
	id: "hestia"
	when: providerConfig: ["github.hestia"]
	tasks: [{
		id:        "hestia.setup"
		label:     "Setup Hestia Nix Cache"
		priority:  4
		dependsOn: ["nix.install"]
		provider: github: {
			uses: "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527"
			with: {
				version:                 "v2.0.0"
				"upstream-cache-filter": "true"
				"drain-timeout":         "900"
			}
		}
	}]
}
