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
	rules: [
		{
			pattern: "*.rs"
			owners: ["@rust-team"]
			section: "Backend"
			description: "Rust source files"
		},
		{
			pattern: "*.ts"
			owners: ["@frontend-team"]
			section: "Frontend"
			description: "TypeScript files"
		},
		{
			pattern: "/docs/**"
			owners: ["@docs-team"]
			description: "Documentation"
		},
	]
}
