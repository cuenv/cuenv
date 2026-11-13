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
let _tasks = tasks

// A pipeline lists the task that reads them.
ci: pipelines: default: tasks: [_tasks.build]

tasks: build: {
	command: "true"
	inputs: [
		"src/**",
		_articles,
		schema.#ModuleFiles & {paths: ["bun.lock", "services/shared/**"]},
	]
}
