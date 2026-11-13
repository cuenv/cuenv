// want: invalid value "vendor/.GIT/config"
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

// No file reference reads the repository's metadata, in any letter case.
tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [schema.#ModuleFiles & {paths: ["vendor/.GIT/config"]}]
}
