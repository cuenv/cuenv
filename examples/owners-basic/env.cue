package _examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "owners-basic"

env: {
	PROJECT_NAME: "owners-test"
}

owners: {
	output: platform: "github"
	defaultOwners: ["@core-team"]
	rules: {
		"rust-files": {
			pattern:     "*.rs"
			owners:      ["@rust-team"]
			section:     "Backend"
			description: "Rust source files"
		}
		"ts-files": {
			pattern:     "*.ts"
			owners:      ["@frontend-team"]
			section:     "Frontend"
			description: "TypeScript files"
		}
		"docs": {
			pattern:     "/docs/**"
			owners:      ["@docs-team"]
			description: "Documentation"
		}
	}
}
