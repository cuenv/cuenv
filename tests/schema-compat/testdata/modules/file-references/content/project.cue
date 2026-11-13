package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "content"

tasks: count: {
	command: "wc"
	args: ["-l", "articles/one.md"]
	inputs: ["articles/**"]
}
