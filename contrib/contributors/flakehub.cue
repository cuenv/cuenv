package contributors

import "github.com/cuenv/cuenv/schema"

// #FlakeHub publishes tagged flakes to FlakeHub.
//
// Active when:
// - ci.provider.github.flakehub is configured
//
// Injects tasks:
// - cuenv:contributor:flakehub.publish: Publishes the current flake to FlakeHub
//
// This is a GitHub-specific contributor and uses DeterminateSystems/flakehub-push.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#FlakeHub]
//	ci: provider: github: flakehub: name: "owner/flake"
#FlakeHub: schema.#Contributor & {
	id: "flakehub"
	when: providerConfig: ["github.flakehub"]
	tasks: [{
		id:        "flakehub.publish"
		label:     "Publish tags to FlakeHub"
		priority:  50
		condition: "on_success"
		provider: github: {
			uses: "DeterminateSystems/flakehub-push@main"
			with: {
				visibility:             "${FLAKEHUB_VISIBILITY}"
				name:                   "${FLAKEHUB_NAME}"
				tag:                    "${FLAKEHUB_TAG}"
				"include-output-paths": "${FLAKEHUB_INCLUDE_OUTPUT_PATHS}"
			}
		}
	}]
}
