package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "npm-workspace-test"

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["build"]
	}
}
