package examples

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/examples/file-references/content:examples"
)

// A path in `inputs` starts from one of three places: this project (a plain
// string), another project (`<pkg>.#Files`, through its import) or the module
// root (`schema.#ModuleFiles`). Every path must match at least one file.
schema.#Project

name: "file-references-app"

tasks: {
	render: schema.#Task & {
		command: "cat"
		args: ["../content/articles/hello.md"]
		inputs: [
			"env.cue",
			content.#Files & {paths: ["articles/**"]},
			schema.#ModuleFiles & {paths: ["flake.lock"]},
		]
	}
}
