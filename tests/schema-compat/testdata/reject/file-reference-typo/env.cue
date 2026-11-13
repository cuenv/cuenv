// want: pths: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [schema.#ModuleFiles & {pths: ["bun.lock"]}]
}
