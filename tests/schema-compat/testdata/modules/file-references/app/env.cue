package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "example.com/consumer/content:cuenv"
)

schema.#Project

name: "app"

// The three places a path can start from. A reference kept in a `let`
// keeps its anchor.
let _articles = content.#Files & {paths: ["articles/**"]}

tasks: build: {
	command: "true"
	inputs: [
		"src/**",
		_articles,
		schema.#ModuleFiles & {paths: ["bun.lock", "services/shared/**"]},
	]
}
