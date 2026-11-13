// want: an input cannot leave the project directory
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

tasks: build: schema.#Task & {
	command: "cargo"
	inputs: ["src/**", "../content/articles/**"]
}
