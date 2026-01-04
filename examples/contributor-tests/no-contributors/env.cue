package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "no-contributors-test"

// No lockfiles in this directory = no workspace contributors should be injected
tasks: {
	build: {
		command: "echo"
		args: ["build"]
	}
	test: {
		command: "echo"
		args: ["test"]
	}
}
