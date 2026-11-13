// want: cuenvProject: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

// A module reference cannot be read as one of a project.
tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [schema.#ModuleFiles & {cuenvProject: "content", paths: ["a"]}]
}
