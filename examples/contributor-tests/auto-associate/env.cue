package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "auto-associate-test"

// This task uses "bun" command and should auto-depend on bun.workspace.setup
tasks: {
	dev: {
		command: "bun"
		args: ["run", "dev"]
	}
	test: {
		command: "bun"
		args: ["test"]
	}
	// This task does NOT use bun, should not get auto-association
	lint: {
		command: "echo"
		args: ["lint"]
	}
}
