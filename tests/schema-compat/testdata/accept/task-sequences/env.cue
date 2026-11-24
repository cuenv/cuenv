package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-sequences"

let _t = tasks

tasks: {
	build: schema.#Task & {command: "echo", args: ["build"], hermetic: false}
	// A sequence of tasks.
	checks: [_t.build, _t.build]
	// A sequence that contains another sequence, and a task that is one.
	release: [_t.checks, _t.build]
	alias: _t.checks
}
