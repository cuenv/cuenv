package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules

// Ignore patterns for generated ignore files
ignore: git: [
	".cache",
	".cargo",
	".cuenv",
	".test",
	"*.vsix",
	"bdd_test_runs",
	"crates/cuengine/vendor",
	"dist",
	"node_modules",
	"result",
	"target",
]

// EditorConfig settings
editorconfig: {
	"*": {
		indent_style:             "tab"
		indent_size:              4
		end_of_line:              "lf"
		charset:                  "utf-8"
		insert_final_newline:     true
		trim_trailing_whitespace: true
	}
	"*.rs": {
		indent_style: "space"
		indent_size:  4
	}
}

// Code ownership rules - aggregated to single CODEOWNERS at repo root
owners: rules: default: {
	pattern: "**"
	owners: ["@rawkode"]
}
