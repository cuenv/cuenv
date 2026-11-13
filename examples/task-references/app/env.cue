package examples

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/examples/task-references/content:examples"
)

// A task of another project is reached through its import: depend on it,
// and read the files it declares in `outputs` through its `outputFiles`
// (which also depends on it). It runs first, in its own project.
schema.#Project

name: "task-references-app"

tasks: {
	show: schema.#Task & {
		command: "cat"
		args: ["../content/dist/hello.txt"]
		dependsOn: [content.tasks.build]
		inputs: ["env.cue", content.tasks.build.outputFiles]
	}
}
