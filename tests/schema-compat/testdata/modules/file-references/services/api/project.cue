package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "api"

tasks: build: {
	command: "true"
	inputs: ["src/**", schema.#ModuleFiles & {paths: ["services/shared/**"]}]
}
