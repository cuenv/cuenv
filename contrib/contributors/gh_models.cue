package contributors

import "github.com/cuenv/cuenv/schema"

// #GhModels installs the GitHub Models CLI extension.
//
// Active when:
// - Any pipeline task uses the `gh models` command
//
// Injects tasks:
// - cuenv:contributor:gh-models.setup: Installs gh-models extension
//
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#GhModels]
#GhModels: schema.#Contributor & {
	id: "gh-models"
	when: taskCommand: ["gh", "models"]
	tasks: [{
		id:       "gh-models.setup"
		label:    "Setup GitHub Models CLI"
		priority: 25
		shell:    false
		command:  "gh extension install github/gh-models"
	}]
}
