package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "scripts"

// Every script file directly inside scripts/ is a script named after the
// file: `cuenv task greet -- "Ada Lovelace"` runs scripts/greet.sh. cuenv
// lists them in scripts.gen.cue, which is committed beside this file.
scriptDirs: [{
	path: "scripts"
	exclude: ["*.lib.sh"]
}]

// What the project adds to a script, by name.
scripts: {
	// A TypeScript file needs a Bun shebang or an interpreter.
	report: interpreter: "bun"
	// Checked by cuenv, then passed as `<name> --greeting <value>`.
	greet: params: {
		positional: [{description: "Who to greet", required: true}]
		greeting: {description: "What to say", default: "hello"}
	}
	// A script depends on a task like any task does.
	count: dependsOn: [_t.hello]
}

let _t = tasks
let _s = scripts

tasks: {
	hello: schema.#Task & {
		command: "echo"
		args: ["declared in CUE"]
	}
	// A task depends on a script by reference.
	summary: schema.#Task & {
		command: "echo"
		args: ["counted"]
		dependsOn: [_s.count]
	}
}

ci: pipelines: default: tasks: [
	_s.greet,
	_t.summary,
]
