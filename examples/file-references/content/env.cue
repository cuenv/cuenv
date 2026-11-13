package examples

import "github.com/cuenv/cuenv/schema"

// A project that holds files other projects read. It declares nothing about
// them: the project that reads them chooses which.
schema.#Project

name: "file-references-content"

tasks: {
	count: schema.#Task & {
		command: "wc"
		args: ["-l", "articles/hello.md"]
		inputs: ["articles/**"]
	}
}
