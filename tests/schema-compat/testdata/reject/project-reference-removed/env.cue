// want: {project:"content",task:"articles"
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

// The removed cross-project reference. Import the project and read
// `<pkg>.tasks.articles.outputFiles` instead.
tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [{project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
}
