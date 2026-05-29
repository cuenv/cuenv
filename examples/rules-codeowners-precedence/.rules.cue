package examples

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
	owners: rules: {
		fallback: {
			pattern: "**"
			owners:  ["@cuenv/maintainers"]
			order:   0
		}
		rust: {
			pattern:     "*.rs"
			owners:      ["@cuenv/rust"]
			description: "Narrower rules appear later so CODEOWNERS precedence wins."
			section:     "Language owners"
			order:       10
		}
	}
}
