package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

// A task reads another task's declared outputs as `outputFiles`, which
// also makes it depend on that task. A pipeline entry and a `dependsOn`
// entry are unified with #TaskNode: a task whose inputs hold such a
// reference must cost there what a task with plain path inputs costs.
let _tasks = tasks

ci: pipelines: default: tasks: [_tasks.site, _tasks.docs.publish]

tasks: {
	generate: {command: "true", inputs: ["schema/**"], outputs: ["gen/types.ts"]}
	site: {
		command: "true"
		inputs: ["src/**", _tasks.generate.outputFiles]
		outputs: ["dist/**"]
	}
	docs: {
		type: "group"
		build: {command: "true", inputs: ["docs/**"], outputs: ["docs/dist/**"]}
		publish: {
			command: "true"
			dependsOn: [_tasks.site]
			inputs: [_tasks.docs.build.outputFiles, _tasks.site.outputFiles]
		}
	}
}
