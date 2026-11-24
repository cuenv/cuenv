package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-inline-task"

// A pipeline entry can be a task defined in place. It is named by its
// description, and it exists only in this pipeline: `cuenv task` does not
// list it.
ci: {
	providers: ["github"]
	pipelines: verify: {
		tasks: [
			_t.build,
			schema.#Task & {
				description: "announce"
				command:     "echo"
				args: ["announced from CI only"]
				dependsOn: [_t.build]
				hermetic: false
			},
		]
		when: branch: "main"
	}
}

tasks: build: schema.#Task & {
	command:  "echo"
	args:     ["building"]
	hermetic: false
}
