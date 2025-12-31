package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "owners-basic"

env: {
	PROJECT_NAME: "owners-test"
}

owners: {
	output: platform: "github"
	rules: {
		"default": {
			pattern:     "*"
			owners:      ["@core-team"]
			description: "Default owners for all files"
			order:       0
		}
		"rust-files": {
			pattern:     "*.rs"
			owners:      ["@rust-team"]
			section:     "Backend"
			description: "Rust source files"
			order:       1
		}
		"ts-files": {
			pattern:     "*.ts"
			owners:      ["@frontend-team"]
			section:     "Frontend"
			description: "TypeScript files"
			order:       2
		}
		"docs": {
			pattern:     "/docs/**"
			owners:      ["@docs-team"]
			description: "Documentation"
			order:       3
		}
	}
}
