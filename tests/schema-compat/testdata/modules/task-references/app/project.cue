package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "example.com/consumer/content:cuenv"
)

schema.#Project

name: "app"

// Tasks of another project, through its import: a dependency on a task and
// on a group, and the files a task declares. A reference kept in a `let`
// is the same task.
let _content = content.tasks
let _tasks = tasks

ci: pipelines: default: tasks: [_tasks.site]

tasks: {
	// A task of this project named like one of `content`'s.
	build: {command: "true", inputs: ["src/**"]}
	site: {
		command: "true"
		dependsOn: [_content.build, content.tasks.checks, _tasks.build]
		inputs: ["src/**", content.tasks.build.outputFiles, _tasks.build.outputFiles]
	}
}
