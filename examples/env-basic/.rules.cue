package _examples

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules

// Ignore patterns for generated ignore files
ignore: {
	git:    ["node_modules/", ".env", "*.log", "target/"]
	docker: ["node_modules/", ".git/", "target/", "*.md"]
}

// EditorConfig settings
editorconfig: {
	"*": {
		indent_style:             "space"
		indent_size:              4
		end_of_line:              "lf"
		charset:                  "utf-8"
		insert_final_newline:     true
		trim_trailing_whitespace: true
	}
	"*.md": {
		trim_trailing_whitespace: false
	}
}
