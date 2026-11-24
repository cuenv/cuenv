package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-comprehensions"

// A reusable script task: the mode is chosen where it is used.
#Script: schema.#Task & {
	args: ["--strict"]
	if (_flag) {
		env: STRICT: "1"
	}
	_flag: *true | bool
}

tasks: {
	// An `if` that sets fields of a command task.
	conditional: schema.#Task & {
		command: "echo"
		if true {env: A: "1"}
	}
	// A `for` that sets fields of a command task.
	looped: schema.#Task & {
		command: "echo"
		for key, value in {B: "2", C: "3"} {env: (key): value}
	}
	// A comprehension in a script task.
	scripted: schema.#Task & {
		script: "echo hi"
		if true {env: D: "4"}
	}
	// A task built from a definition that was written before its mode.
	reused: #Script & {command: "lint"}
}
