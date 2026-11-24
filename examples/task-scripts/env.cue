package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-scripts"

// Every script file in scripts/ is a task named after the file:
// `cuenv task greet -- "Ada Lovelace"` runs scripts/greet.sh.
taskScripts: [{
	path: "scripts"
	exclude: ["*.lib.sh"]
	// A TypeScript file needs a Bun shebang or an explicit interpreter.
	interpreters: "report.ts": "bun"
}]

let _t = tasks

tasks: hello: schema.#Task & {
	command: "echo"
	args: ["declared in CUE"]
}

ci: pipelines: scripts: tasks: [
	{taskScript: "greet"},
	_t.hello,
]
