package stages

import "github.com/cuenv/cuenv/schema"

// #GhModels installs the GitHub Models CLI extension.
//
// Active when:
// - Any pipeline task uses the `gh models` command
//
// Contributes to Setup phase with priority 25 (after cuenv, before task execution).
//
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: contributors: [stages.#GhModels]
#GhModels: schema.#Contributor & {
	id: "gh-models"
	when: {
		// Active if any pipeline task uses gh models command
		taskCommand: ["gh", "models"]
	}
	tasks: [{
		id:       "setup-gh-models"
		phase:    "setup"
		label:    "Setup GitHub Models CLI"
		priority: 25
		shell:    false
		command:  "gh extension install github/gh-models"
	}]
}
