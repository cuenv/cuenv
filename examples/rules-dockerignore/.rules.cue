package examples

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
	ignore: docker: [
		"target/",
		".git/",
		"*.md",
	]
}
