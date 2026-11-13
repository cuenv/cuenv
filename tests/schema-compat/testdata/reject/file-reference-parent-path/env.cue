// want: invalid value "../shared/**"
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [schema.#ModuleFiles & {paths: ["../shared/**"]}]
}
