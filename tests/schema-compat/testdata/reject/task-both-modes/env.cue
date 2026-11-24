// want: empty disjunction
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-both-modes"

// A task runs a command or a script, not both.
tasks: both: schema.#Task & {command: "echo", script: "echo hi"}
