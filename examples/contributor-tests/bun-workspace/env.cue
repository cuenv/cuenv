package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "bun-workspace-test"

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["build"]
	}
}
