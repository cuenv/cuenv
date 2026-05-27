package examples

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
	ignore: git: [
		"target/",
		".env",
		"*.log",
	]
}
