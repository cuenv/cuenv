package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "no-contributors-test"

// No lockfiles in this directory = no workspace contributors should be injected
tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["build"]
	}
	test: schema.#Task & {
		command: "echo"
		args: ["test"]
	}
}
