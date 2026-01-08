package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "auto-associate-test"

// This task uses "bun" command and should auto-depend on bun.workspace.setup
tasks: {
	dev: schema.#Task & {
		command: "bun"
		args: ["run", "dev"]
	}
	test: schema.#Task & {
		command: "bun"
		args: ["test"]
	}
	// This task does NOT use bun, should not get auto-association
	lint: schema.#Task & {
		command: "echo"
		args: ["lint"]
	}
}
